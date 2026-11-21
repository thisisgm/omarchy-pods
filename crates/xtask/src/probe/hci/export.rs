//! The `probe hci` command: reads a trace through the store's bounded read and writes its tables into a new run.

use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use pods_proto::packets::{ATT_PSM, CONTROL_PSM};
use pods_proto::sdulog::{Direction, LogError, format_line};

use super::l2cap::Counts;
use super::{Channel, Channels, HciRecord, Kind, TraceError, channels, records};
use crate::probe::a2dp::{self, Codec, CodecKind, Second, Unread, VENDOR_CODEC};
use crate::probe::store::{self, Run, Session, StoreError};

/// The log of the first channel on the control PSM; a second channel gets `sdu-2.log`.
const CONTROL_LOG: &str = "sdu";
/// The log of the first channel on the attribute PSM.
const ATT_LOG: &str = "att";
/// The table of connection requests.
const REQUESTS_NAME: &str = "psm.tsv";
/// The table of the music stream, one row a second.
const SECONDS_NAME: &str = "a2dp.tsv";
/// The codecs the pods offered and the one that was set.
const CODECS_NAME: &str = "avdtp.txt";
/// The counts, which the command also prints.
const LINKS_NAME: &str = "links.txt";
/// The column names of `psm.tsv`.
const REQUESTS_HEADER: &str = "at_ns\tside\tpsm\tresult\thandle\thost_mtu\tpods_mtu";
/// The column names of `a2dp.tsv`.
const SECONDS_HEADER: &str =
    "second\tmedia_packets\tmedia_bytes\trtp_gaps\tlargest_interval_ns\tavdtp_suspend\tavdtp_start";
/// What a column holds when the trace has no value for it.
const ABSENT: &str = "-";
/// The smallest attribute MTU that fits a 438 byte notification behind its 3 byte header (Core 6.1 Vol 3 Part F 3.4.7.1).
const ATT_MTU_NEEDED: u16 = 441;

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

fn side(direction: Direction) -> &'static str {
    match direction {
        Direction::ToPods => "host",
        Direction::FromPods => "pods",
    }
}

/// One channel's packets as capture log lines, and the line that counts what the log format could not hold.
fn log(name: &str, channel: &Channel) -> Result<(String, String), ExportError> {
    let mut lines = Vec::new();
    let (mut empty, mut too_long) = (0_u64, 0_u64);
    for sdu in &channel.sdus {
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
    let counted =
        format!("log {name} written={written} refused_empty={empty} refused_too_long={too_long}");
    Ok((text(&lines), counted))
}

/// Every connection request with who sent it, how it was answered and the MTU option each side sent for its channel.
fn requests_table(found: &Channels) -> String {
    let mut lines = vec![REQUESTS_HEADER.to_owned()];
    let or_absent = |value: Option<String>| value.unwrap_or_else(|| ABSENT.to_owned());
    for request in &found.requests {
        let channel = request.channel.and_then(|index| found.list.get(index));
        let result = request.result.map(|result| format!("{result:#06x}"));
        let host_mtu = channel.and_then(|channel| channel.host_mtu);
        let pods_mtu = channel.and_then(|channel| channel.pods_mtu);
        lines.push(format!(
            "{at_ns}\t{side}\t{psm:#06x}\t{result}\t{handle:#06x}\t{host_mtu}\t{pods_mtu}",
            at_ns = request.at_ns,
            side = side(request.direction),
            psm = request.psm,
            result = or_absent(result),
            handle = request.handle,
            host_mtu = or_absent(host_mtu.map(|mtu| mtu.to_string())),
            pods_mtu = or_absent(pods_mtu.map(|mtu| mtu.to_string())),
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

fn codecs_list(found: &Channels) -> String {
    let mut lines: Vec<String> = a2dp::capabilities(found)
        .iter()
        .map(|codec| codec_line("capability", codec))
        .collect();
    lines.push(a2dp::configuration(found).map_or_else(
        || "configuration none".to_owned(),
        |codec| codec_line("configuration", &codec),
    ));
    text(&lines)
}

/// The counts of `links.txt`: where zero is, the synchronous connections, and everything the reader could not use.
fn link_lines(zero_ns: u64, records: &[HciRecord], found: &Channels) -> Vec<String> {
    let synchronous = |wanted: Option<bool>| {
        records
            .iter()
            .filter(|record| match record.kind {
                Kind::SynchronousConnection { opened } => wanted.is_none_or(|want| want == opened),
                Kind::AclSent | Kind::AclReceived | Kind::Disconnection { .. } => false,
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
    let Unread {
        fragmented_signals,
        malformed_signals: avdtp_malformed_signals,
        not_rtp,
        other_channels,
    } = a2dp::unread(found);
    let mut lines = vec![
        format!("realtime_at_zero_ns {zero_ns}"),
        format!("synchronous_connection_events {}", synchronous(None)),
        format!("synchronous_connections_opened {}", synchronous(Some(true))),
    ];
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
        ("avdtp_fragmented_signals", fragmented_signals),
        ("avdtp_malformed_signals", avdtp_malformed_signals),
        ("avdtp_other_channels", other_channels),
        ("media_packets_not_rtp", not_rtp),
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
    let offered = found
        .list
        .iter()
        .filter(|channel| channel.psm == ATT_PSM)
        .filter_map(|channel| channel.pods_mtu);
    for mtu in offered.filter(|mtu| *mtu < ATT_MTU_NEEDED) {
        lines.push(format!("att_pods_mtu_under_{ATT_MTU_NEEDED} {mtu}"));
    }
    lines
}

/// Every file of the run, from records whose stamps are counted from the first one kept.
pub(crate) fn render(records: Vec<HciRecord>) -> Result<Vec<Output>, ExportError> {
    let zero_ns = records.first().map_or(0, |record| record.at_ns);
    let records: Vec<HciRecord> = records
        .into_iter()
        .map(|record| HciRecord {
            at_ns: record.at_ns.saturating_sub(zero_ns),
            ..record
        })
        .collect();
    let found = channels(&records);
    let mut links = link_lines(zero_ns, &records, &found);
    let mut outputs = Vec::new();
    for (psm, stem) in [(CONTROL_PSM, CONTROL_LOG), (ATT_PSM, ATT_LOG)] {
        let on_psm = found.list.iter().filter(|channel| channel.psm == psm);
        for (number, channel) in (1_usize..).zip(on_psm) {
            let name = if number == 1 {
                format!("{stem}.log")
            } else {
                format!("{stem}-{number}.log")
            };
            let (text, counted) = log(&name, channel)?;
            links.push(counted);
            outputs.push(Output { name, text });
        }
    }
    for (name, text) in [
        (REQUESTS_NAME, requests_table(&found)),
        (SECONDS_NAME, seconds_table(&a2dp::media(&found))),
        (CODECS_NAME, codecs_list(&found)),
        (LINKS_NAME, text(&links)),
    ] {
        outputs.push(Output {
            name: name.to_owned(),
            text,
        });
    }
    Ok(outputs)
}

/// Reads `trace`, then creates the run and writes its files; a trace that is refused creates nothing.
pub(crate) fn export(
    root: &Path,
    trace: &Path,
    stamp: &str,
    run: &str,
) -> Result<String, ExportError> {
    let bytes = store::read_trace(trace)?;
    let records = records(&bytes).map_err(|source| ExportError::Trace {
        path: trace.to_path_buf(),
        source,
    })?;
    let outputs = render(records)?;
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
    Ok(summary)
}

#[cfg(test)]
mod tests;
