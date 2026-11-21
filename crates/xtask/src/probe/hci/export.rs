//! The `probe hci` command: reads a trace through the store's bounded read and writes its tables into a new run.

use std::collections::BTreeSet;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use pods_proto::packets::{ATT_PSM, CONTROL_PSM};
use pods_proto::sdulog::{Direction, LogError, Record, format_line};

use super::l2cap::Counts;
use super::{
    Carries, Channel, Channels, Cut, HciRecord, Kind, Read, Records, TraceError, channels,
    channels_by_content, read,
};
use crate::probe::a2dp::{self, AVDTP_PSM, Codec, CodecKind, Music, Second, Unread, VENDOR_CODEC};
use crate::probe::store::{self, Run, Session, StoreError};

/// The log of the first channel on the control PSM; a second channel gets `sdu-2.log`.
const CONTROL_LOG: &str = "sdu";
/// The log of the first channel on the attribute PSM.
const ATT_LOG: &str = "att";
/// The table of connection requests.
const REQUESTS_NAME: &str = "psm.tsv";
/// The table of the music stream of the first pods' link, one row a second; a second link gets `a2dp-2.tsv`.
const SECONDS_TABLE: &str = "a2dp";
/// The codecs the pods offered on that link and the one that was set.
const CODECS_LIST: &str = "avdtp";
/// The counts, which the command also prints.
const LINKS_NAME: &str = "links.txt";
/// The column names of `psm.tsv`.
const REQUESTS_HEADER: &str =
    "at_ns\tside\tpsm\tresult\thandle\thost_mtu\tpods_mtu\thost_mode\tpods_mode";
/// The column names of `a2dp.tsv`.
const SECONDS_HEADER: &str =
    "second\tmedia_packets\tmedia_bytes\trtp_gaps\tlargest_interval_ns\tavdtp_suspend\tavdtp_start";
/// What a column holds when the trace has no value for it.
const ABSENT: &str = "-";
/// A trace is refused above 512 MiB; it holds the music stream of a whole session, which the store's capture bound is too small for.
const MAX_TRACE_BYTES: u64 = 512 * 1024 * 1024;
/// The smallest attribute MTU that fits a 438 byte notification behind its 3 byte header (Core 6.1 Vol 3 Part F 3.4.7.1).
const ATT_MTU_NEEDED: u16 = 441;

/// What the command did: the text of `links.txt`, and where the trace was cut when it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Exported {
    pub summary: String,
    pub cut: Option<Cut>,
}

/// One file of the run: its name and its whole text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Output {
    pub name: String,
    pub text: String,
}

/// Why the command failed; each names the file and the rule, never a byte of the trace.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ExportError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(
        "{source}; create the file yourself with mode 600 before btmon -w writes into it, or make this one yours with chown and chmod 600"
    )]
    NotPrivate { source: StoreError },
    #[error("{}: {source}", .path.display())]
    Trace { path: PathBuf, source: TraceError },
    #[error("could not write {}: {source}", .path.display())]
    Write { path: PathBuf, source: io::Error },
    #[error("could not write a line of {name}: {source}")]
    Log { name: String, source: LogError },
}

/// Bytes as lowercase hex with nothing between them.
struct Hex<'a>(&'a [u8]);

impl fmt::Display for Hex<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

/// Lines joined into the text of a file, each ended by a newline.
fn text(lines: &[String]) -> String {
    lines
        .iter()
        .flat_map(|line| [line.as_str(), "\n"])
        .collect()
}

// Sample output for ("sdu", 1, "log") and for ("a2dp", 2, "tsv"): `sdu.log` and `a2dp-2.tsv`.
fn numbered(stem: &str, number: usize, extension: &str) -> String {
    if number == 1 {
        format!("{stem}.{extension}")
    } else {
        format!("{stem}-{number}.{extension}")
    }
}

fn side(direction: Direction) -> &'static str {
    match direction {
        Direction::ToPods => "host",
        Direction::FromPods => "pods",
    }
}

/// An L2CAP mode as `psm.tsv` and `links.txt` write it: 0x00 is basic, 0x03 enhanced retransmission, 0x04 streaming.
fn mode_text(mode: u8) -> String {
    format!("{mode:#04x}")
}

/// Packets as capture log lines, and the line that names their link and counts what the log format could not hold.
fn log(name: &str, handle: u16, sdus: &[Record]) -> Result<(String, String), ExportError> {
    let mut lines = Vec::new();
    let (mut empty, mut too_long) = (0_u64, 0_u64);
    for sdu in sdus {
        match format_line(sdu.at_ns, sdu.direction, &sdu.sdu) {
            Ok(line) => lines.push(line),
            Err(LogError::Empty) => empty = empty.saturating_add(1),
            Err(LogError::TooLong { .. }) => too_long = too_long.saturating_add(1),
            Err(
                source @ (LogError::MissingField
                | LogError::BadStamp
                | LogError::BadDirection
                | LogError::BadHex { .. }),
            ) => {
                return Err(ExportError::Log {
                    name: name.to_owned(),
                    source,
                });
            }
        }
    }
    let written = lines.len();
    let counted = format!(
        "log {name} handle={handle:#06x} written={written} refused_empty={empty} refused_too_long={too_long}"
    );
    Ok((text(&lines), counted))
}

/// Every connection request with who sent it, how it was answered and the MTU and the mode each side asked for its channel.
fn requests_table(found: &Channels) -> String {
    let mut lines = vec![REQUESTS_HEADER.to_owned()];
    let or_absent = |value: Option<String>| value.unwrap_or_else(|| ABSENT.to_owned());
    for request in &found.requests {
        let channel = request.channel.and_then(|index| found.list.get(index));
        let result = request.result.map(|result| format!("{result:#06x}"));
        let host_mtu = channel.and_then(|channel| channel.host_mtu);
        let pods_mtu = channel.and_then(|channel| channel.pods_mtu);
        let host_mode = channel.and_then(|channel| channel.host_mode);
        let pods_mode = channel.and_then(|channel| channel.pods_mode);
        lines.push(format!(
            "{at_ns}\t{side}\t{psm:#06x}\t{result}\t{handle:#06x}\t{host_mtu}\t{pods_mtu}\t{host_mode}\t{pods_mode}",
            at_ns = request.at_ns,
            side = side(request.direction),
            psm = request.psm,
            result = or_absent(result),
            handle = request.handle,
            host_mtu = or_absent(host_mtu.map(|mtu| mtu.to_string())),
            pods_mtu = or_absent(pods_mtu.map(|mtu| mtu.to_string())),
            host_mode = or_absent(host_mode.map(mode_text)),
            pods_mode = or_absent(pods_mode.map(mode_text)),
        ));
    }
    text(&lines)
}

fn seconds_table(seconds: &[Second]) -> String {
    let mut lines = vec![SECONDS_HEADER.to_owned()];
    for row in seconds {
        lines.push(format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            row.at_s,
            row.packets,
            row.bytes,
            row.gaps,
            row.largest_interval_ns,
            row.suspends,
            row.starts
        ));
    }
    text(&lines)
}

fn codec_line(label: &str, codec: &Codec) -> String {
    let kind = match codec.kind {
        CodecKind::Standard(codec_type) => format!("type={codec_type:#04x}"),
        CodecKind::Vendor {
            vendor_id,
            codec_id,
        } => format!("type={VENDOR_CODEC:#04x} vendor={vendor_id:#010x} codec={codec_id:#06x}"),
    };
    format!("{label} {kind} bytes={}", Hex(&codec.bytes))
}

fn codecs_list(music: &Music) -> String {
    let mut lines: Vec<String> = music
        .capabilities
        .iter()
        .map(|codec| codec_line("capability", codec))
        .collect();
    lines.push(music.configuration.as_ref().map_or_else(
        || "configuration none".to_owned(),
        |codec| codec_line("configuration", codec),
    ));
    text(&lines)
}

/// The first control channel of each link that opened one; such a link is the pods', the one mark a trace of any host holds, while an address needs a connection event the trace may start after.
fn pods_links(found: &Channels) -> Vec<&Channel> {
    let mut seen = BTreeSet::new();
    let on_control = |channel: &&Channel| channel.psm == CONTROL_PSM;
    let firsts = found.list.iter().filter(on_control);
    firsts.filter(|channel| seen.insert(channel.link)).collect()
}

/// The two music files of the pods' link `first` is on, read in one pass, and the line that names that link and counts what was not read.
fn music_tables(number: usize, first: &Channel, found: &Channels) -> (String, [Output; 2]) {
    let music = a2dp::music(found, first.link);
    let Unread {
        fragmented_signals,
        malformed_signals,
        not_rtp,
        other_channels,
    } = music.unread;
    let seconds = numbered(SECONDS_TABLE, number, "tsv");
    let codecs = numbered(CODECS_LIST, number, "txt");
    let counted = format!(
        "music {seconds} {codecs} handle={:#06x} avdtp_fragmented_signals={fragmented_signals} avdtp_malformed_signals={malformed_signals} avdtp_other_channels={other_channels} media_packets_not_rtp={not_rtp}",
        first.handle
    );
    let tables = [
        Output {
            name: seconds,
            text: seconds_table(&music.seconds),
        },
        Output {
            name: codecs,
            text: codecs_list(&music),
        },
    ];
    (counted, tables)
}

/// Where zero is and how often the stamps of the records step back, as `links.txt` states them.
#[derive(Debug, Clone, Copy)]
struct Clock {
    zero_ns: u64,
    steps_back: u64,
}

/// Zero is the earliest stamp, so no stamp is moved; a stamp earlier than the one before it is a step back of the recorder's wall clock.
fn clock(kept: &[HciRecord]) -> Clock {
    let zero_ns = kept.iter().map(|record| record.at_ns).min().unwrap_or(0);
    let steps_back = kept
        .windows(2)
        .filter(|pair| matches!(pair, [before, after] if after.at_ns < before.at_ns))
        .count();
    Clock {
        zero_ns,
        steps_back: u64::try_from(steps_back).unwrap_or(u64::MAX),
    }
}

/// What `links.txt` opens with for a btsnoop trace: where it was cut, when it was.
fn cut_line(cut: Option<Cut>) -> Vec<String> {
    let line = |cut: Cut| {
        format!(
            "{} record={} offset={}",
            cut.damage.key(),
            cut.record,
            cut.offset
        )
    };
    cut.map(line).into_iter().collect()
}

/// What `links.txt` opens with for a `PacketLogger` trace: its synchronous data records, which hold no audio, by who sent them.
fn synchronous_data_lines(kept: &[HciRecord]) -> Vec<String> {
    let from = |side: Direction| {
        let sent = |record: &&HciRecord| record.kind == Kind::SynchronousData(side);
        kept.iter().filter(sent).count()
    };
    vec![
        format!("synchronous_packets_from_host {}", from(Direction::ToPods)),
        format!(
            "synchronous_packets_from_pods {}",
            from(Direction::FromPods)
        ),
    ]
}

/// The counts of `links.txt`: what its kind of trace opens it with, the clock, the synchronous connections, and everything the reader could not use.
fn link_lines(
    mut lines: Vec<String>,
    clock: Clock,
    records: &[HciRecord],
    found: &Channels,
    pods: &[&Channel],
) -> Vec<String> {
    let synchronous = |wanted: Option<bool>| {
        records
            .iter()
            .filter(|record| match record.kind {
                Kind::SynchronousConnection { opened } => wanted.is_none_or(|want| want == opened),
                Kind::AclSent
                | Kind::AclReceived
                | Kind::Disconnection { .. }
                | Kind::SynchronousData(_) => false,
            })
            .count()
    };
    let Counts {
        acl_packets,
        malformed_acl_packets,
        continuations_without_start,
        unfinished_pdus,
        malformed_pdus,
        malformed_signals,
        responses_without_request,
        fixed_channel_pdus,
        requests_over_limit,
        unmapped_over_limit,
    } = found.counts;
    let pods_links: BTreeSet<usize> = pods.iter().map(|first| first.link).collect();
    let on_other_links =
        |channel: &&Channel| channel.psm == AVDTP_PSM && !pods_links.contains(&channel.link);
    let avdtp_channels_on_other_links = found.list.iter().filter(on_other_links).count();
    let not_basic_mode_pdus = found.list.iter().fold(0_u64, |pdus, channel| {
        pdus.saturating_add(channel.not_basic_pdus)
    });
    lines.extend([
        format!("realtime_at_zero_ns {}", clock.zero_ns),
        format!("stamps_stepping_back {}", clock.steps_back),
        format!("synchronous_connection_events {}", synchronous(None)),
        format!("synchronous_connections_opened {}", synchronous(Some(true))),
    ]);
    for (name, count) in [
        ("acl_packets", acl_packets),
        ("malformed_acl_packets", malformed_acl_packets),
        ("continuations_without_start", continuations_without_start),
        ("unfinished_pdus", unfinished_pdus),
        ("malformed_pdus", malformed_pdus),
        ("malformed_signals", malformed_signals),
        ("responses_without_request", responses_without_request),
        ("fixed_channel_pdus", fixed_channel_pdus),
        ("requests_over_limit", requests_over_limit),
        ("unmapped_over_limit", unmapped_over_limit),
        ("not_basic_mode_pdus", not_basic_mode_pdus),
        (
            "avdtp_channels_on_other_links",
            u64::try_from(avdtp_channels_on_other_links).unwrap_or(u64::MAX),
        ),
    ] {
        lines.push(format!("{name} {count}"));
    }
    for unmapped in &found.unmapped {
        lines.push(format!(
            "unmapped_channel handle={:#06x} cid={:#06x} sent_by={} pdus={}",
            unmapped.handle,
            unmapped.cid,
            side(unmapped.direction),
            unmapped.pdus
        ));
    }
    for carried in &found.by_content {
        let carries = match carried.carries {
            Carries::Accessory => "accessory",
            Carries::Attributes => "attributes",
        };
        for half in &carried.halves {
            lines.push(format!(
                "channel_by_content handle={:#06x} cid={:#06x} sent_by={} carries={carries} pdus={}",
                carried.handle,
                half.cid,
                side(half.direction),
                half.pdus
            ));
        }
    }
    let or_absent = |mode: Option<u8>| mode.map_or_else(|| ABSENT.to_owned(), mode_text);
    for channel in found.list.iter().filter(|channel| !channel.is_basic()) {
        lines.push(format!(
            "channel_not_read handle={:#06x} psm={:#06x} host_mode={} pods_mode={} pdus={} reason=l2cap_mode_is_not_basic",
            channel.handle,
            channel.psm,
            or_absent(channel.host_mode),
            or_absent(channel.pods_mode),
            channel.not_basic_pdus
        ));
    }
    // The attribute MTU of a classic link is the smaller of the two sides' L2CAP MTUs, so either side's offer can be too small.
    for channel in found.list.iter().filter(|channel| channel.psm == ATT_PSM) {
        for (side, offered) in [("host", channel.host_mtu), ("pods", channel.pods_mtu)] {
            if let Some(mtu) = offered.filter(|mtu| *mtu < ATT_MTU_NEEDED) {
                lines.push(format!("att_{side}_mtu_under_{ATT_MTU_NEEDED} {mtu}"));
            }
        }
    }
    lines
}

/// Every file of the run, from records whose stamps are counted from the earliest one kept.
pub(crate) fn render(read: Read) -> Result<Vec<Output>, ExportError> {
    type Mapper = fn(&[HciRecord]) -> Channels;
    let (kept, opening, map): (_, _, Mapper) = match read {
        Read::Btsnoop(Records { kept, cut }) => (kept, cut_line(cut), channels),
        Read::PacketLogger(kept) => {
            let opening = synchronous_data_lines(&kept);
            (kept, opening, channels_by_content)
        }
    };
    let clock = clock(&kept);
    let records: Vec<HciRecord> = kept
        .into_iter()
        .map(|record| HciRecord {
            at_ns: record.at_ns.saturating_sub(clock.zero_ns),
            ..record
        })
        .collect();
    let found = map(&records);
    let pods = pods_links(&found);
    let mut links = link_lines(opening, clock, &records, &found, &pods);
    let mut outputs = Vec::new();
    for (psm, carries, stem) in [
        (CONTROL_PSM, Carries::Accessory, CONTROL_LOG),
        (ATT_PSM, Carries::Attributes, ATT_LOG),
    ] {
        let opened = found.list.iter().filter(|channel| channel.psm == psm);
        let opened = opened.map(|channel| {
            let basic = channel.is_basic().then_some(&channel.sdus);
            (channel.handle, basic)
        });
        let told = found.by_content.iter();
        let told = told.filter(|told| told.carries == carries);
        let told = told.map(|told| (told.handle, Some(&told.sdus)));
        for (number, (handle, sdus)) in (1_usize..).zip(opened.chain(told)) {
            // A channel in another mode keeps its number and gets no log; links.txt says why.
            let Some(sdus) = sdus else {
                continue;
            };
            let name = numbered(stem, number, "log");
            let (text, counted) = log(&name, handle, sdus)?;
            links.push(counted);
            outputs.push(Output { name, text });
        }
    }
    outputs.push(Output {
        name: REQUESTS_NAME.to_owned(),
        text: requests_table(&found),
    });
    // Each pods' link gets tables of its own, since a row that sums two links describes neither.
    for (number, first) in (1_usize..).zip(&pods) {
        let (counted, tables) = music_tables(number, first, &found);
        links.push(counted);
        outputs.extend(tables);
    }
    if pods.is_empty() {
        links.push(format!(
            "music_tables_not_written reason=no_link_opened_psm_{CONTROL_PSM:#06x}"
        ));
    }
    outputs.push(Output {
        name: LINKS_NAME.to_owned(),
        text: text(&links),
    });
    Ok(outputs)
}

/// Adds what to do to the two refusals a trace made by btmon alone runs into: a file of root's, or one others can read.
fn refused_read(source: StoreError) -> ExportError {
    let not_private = matches!(
        source,
        StoreError::WrongOwner { .. } | StoreError::NotOwnerOnly { .. }
    );
    if not_private {
        ExportError::NotPrivate { source }
    } else {
        ExportError::Store(source)
    }
}

/// Reads `trace`, then creates the run and writes its files; a trace that is refused creates nothing.
pub(crate) fn export(
    root: &Path,
    trace: &Path,
    stamp: &str,
    run: &str,
) -> Result<Exported, ExportError> {
    // A run name the store refuses must leave no session directory behind, so it is judged before anything is read or made.
    store::check_run_name(run)?;
    let bytes = store::read_trace(trace, MAX_TRACE_BYTES).map_err(refused_read)?;
    let read = read(&bytes).map_err(|source| ExportError::Trace {
        path: trace.to_path_buf(),
        source,
    })?;
    // The kept records hold copies of what they need, so the trace leaves memory before the tables are built.
    drop(bytes);
    let cut = match &read {
        Read::Btsnoop(records) => records.cut,
        Read::PacketLogger(_) => None,
    };
    let outputs = render(read)?;
    let session = Session::open(root, stamp)?;
    let created = Run::create(&session, run)?;
    let mut summary = String::new();
    for output in outputs {
        let mut file = created.create_file(&output.name)?;
        file.write_all(output.text.as_bytes())
            .map_err(|source| ExportError::Write {
                path: session.path().join(run).join(&output.name),
                source,
            })?;
        if output.name == LINKS_NAME {
            summary = output.text;
        }
    }
    Ok(Exported { summary, cut })
}

#[cfg(test)]
mod tests;
