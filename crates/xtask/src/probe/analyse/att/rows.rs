//! The rows the dictation stream is reported in: the whole log first, then each session.

use std::collections::BTreeMap;
use std::fmt::Display;

use pods_proto::dictation::{AUDIO_HANDLE, TOC_CELT_WIDEBAND_20MS_MONO};

use super::super::input::ATT_LOG_FILE;
use super::super::lead::{LEAD_MIN_PACKETS, LEAD_WINDOW_NS, Lead};
use super::super::rates::{ClockEstimate, Thin};
use super::super::report::{
    BIT_PER_SECOND, BYTES, HZ, MS, NOTE_SEPARATOR, PACKETS, REFUSED, Row, labelled,
};
use super::super::stats::{millis, seconds};
use super::load::LoadedAtt;
use super::session::{
    AfterStop, FLOOR, FRAME_MS, Heard, REFERENCE_HZ, SAMPLES_PER_FRAME, Session, Skip, TAIL_MS,
    after_burst, bitrate, frame_clock, lead_of,
};

/// The flag byte of a frame the pods mark, and of one they do not.
const FLAGS: (u8, u8) = (0x80, 0x00);
const FRAMES: &str = "frames";
const SKIPS: &str = "skips";
/// The most skips of one counter that get a row each; a broken log skips at every step, and one more row counts the rest.
const SKIP_ROWS: usize = 20;

/// The rows of the whole log; `outside` counts the audio notifications in no session and `cut` says whether the last line was left out.
pub(super) fn log_rows(
    whole: &LoadedAtt,
    sessions: &[Session],
    outside: usize,
    cut: bool,
) -> Vec<Row> {
    let frames = whole.frames().count();
    let mismatched = whole.audio.iter().filter(|packet| !packet.length_matches);
    let stray = whole.audio.iter().filter(|packet| packet.trailing > 0);
    let trailing = whole.audio.iter().fold(0_usize, |bytes, packet| {
        bytes.saturating_add(packet.trailing)
    });
    let cut_note = format!(
        "last line of {ATT_LOG_FILE} without its newline, left out: a capture killed while writing"
    );
    let audio_note = format!("notifications on handle {AUDIO_HANDLE:#06x} that open with 01 00");
    let mut rows = vec![
        Row::new("att_log_cut_lines", &u8::from(cut), "lines", &cut_note),
        Row::new("att_packets_from_pods", &whole.from_pods, PACKETS, ""),
        Row::new(
            "att_packets_to_pods",
            &whole.to_pods,
            PACKETS,
            "written by the host; in no figure",
        ),
        Row::new(
            "audio_notifications",
            &whole.audio.len(),
            PACKETS,
            &audio_note,
        ),
        Row::new(
            "audio_frames",
            &frames,
            FRAMES,
            "whole frames in the audio notifications",
        ),
        Row::new(
            "parse_failures",
            &whole.parse_failures,
            PACKETS,
            "from the pods and read by no parser: cut before its fixed fields, a list with no whole entry, or a value on the audio handle without the 01 00 marker",
        ),
        Row::new(
            "length_mismatches",
            &mismatched.count(),
            PACKETS,
            "audio notifications whose declared length is not the length of what follows it",
        ),
        Row::new(
            "trailing_bytes",
            &trailing,
            BYTES,
            &format!(
                "after the last whole frame; in {} notifications",
                stray.count()
            ),
        ),
        figure(
            "largest_pdu",
            BYTES,
            whole.largest_pdu.ok_or("no packet from the pods"),
            "the longest packet from the pods",
        ),
        frames_per_packet_row(whole),
        codec_row(whole),
    ];
    rows.extend(flag_rows(whole));
    rows.extend(skip_rows(sessions));
    rows.extend(outside_rows(sessions, outside));
    rows.push(Row::new(
        "handles_listed",
        &whole.handles.len(),
        "handles",
        "by the walk of the table and the read of the declarations",
    ));
    let asked = whole
        .requests
        .values()
        .fold(0_u32, |sum, count| sum.saturating_add(*count));
    rows.push(Row::new(
        "requests_from_pods",
        &asked,
        PACKETS,
        "requests the pods made of the host",
    ));
    rows.extend(whole.requests.iter().map(|(opcode, count)| {
        Row::new(
            &format!("request_opcode_{opcode:#04x}"),
            count,
            PACKETS,
            "by ATT opcode",
        )
    }));
    rows
}

/// The audio no session takes: the notifications of the whole log, and the frames left after a stop mark.
fn outside_rows(sessions: &[Session], outside: usize) -> [Row; 2] {
    let left = sessions.iter().map(|session| session.tail.left);
    [
        Row::new(
            "audio_outside_a_session",
            &outside,
            PACKETS,
            "audio notifications no session takes: not between a dictation-start mark and the mark that ends it, and not among those session_frames_after_stop counts; in no rate and no skip count",
        ),
        Row::new(
            "frames_after_stop_outside",
            &left.fold(0_usize, usize::saturating_add),
            FRAMES,
            &format!(
                "whole frames after a dictation-stop mark and before the next dictation-start mark that the session does not take: all of them where the session holds no frame before the mark, else those from the first audio notification that comes {TAIL_MS} ms or more after the mark, holds no whole frame, or holds a frame whose counter is not one on from the frame before"
            ),
        ),
    ]
}

/// A row whose figure may not be formed: the figure with `note`, or `refused` with the reason.
fn figure<T: Display>(key: &str, unit: &'static str, found: Result<T, &str>, note: &str) -> Row {
    match found {
        Ok(found) => Row::new(key, &found, unit, note),
        Err(why) => Row::new(key, &REFUSED, unit, why),
    }
}

/// A histogram as `1:2774 4:6 5:33`, each value with how often it occurs.
fn histogram<K: Display>(counts: &BTreeMap<K, u32>) -> String {
    let items: Vec<String> = counts
        .iter()
        .map(|(value, count)| format!("{value}:{count}"))
        .collect();
    items.join(" ")
}

fn tally<K: Ord>(values: impl Iterator<Item = K>) -> BTreeMap<K, u32> {
    let mut counts = BTreeMap::new();
    for value in values {
        let seen = counts.entry(value).or_insert(0_u32);
        *seen = seen.saturating_add(1);
    }
    counts
}

fn frames_per_packet_row(whole: &LoadedAtt) -> Row {
    let counts = tally(whole.audio.iter().map(|packet| packet.frames.len()));
    let found = if counts.is_empty() {
        Err("no audio notification")
    } else {
        Ok(histogram(&counts))
    };
    figure("frames_per_packet", "packets by frame count", found, "")
}

/// The frames whose first byte is not the one table of contents byte seen so far, counted and listed by that byte.
fn codec_row(whole: &LoadedAtt) -> Row {
    let odd = whole
        .frames()
        .filter(|frame| frame.toc != Some(TOC_CELT_WIDEBAND_20MS_MONO));
    let first_bytes = tally(odd.map(|frame| {
        frame
            .toc
            .map_or_else(|| "empty".to_owned(), |toc| format!("{toc:#04x}"))
    }));
    let total = first_bytes
        .values()
        .fold(0_u32, |sum, count| sum.saturating_add(*count));
    let rule = format!("first byte is not {TOC_CELT_WIDEBAND_20MS_MONO:#04x}");
    let listed = if first_bytes.is_empty() {
        String::new()
    } else {
        format!("by first byte {}", histogram(&first_bytes))
    };
    Row::new(
        "unexpected_codec",
        &total,
        FRAMES,
        &labelled(&rule, &[&listed]),
    )
}

fn flag_rows(whole: &LoadedAtt) -> [Row; 5] {
    let (flagged, unflagged) = FLAGS;
    let with = |flag: u8| whole.frames().filter(|frame| frame.flag == flag).count();
    let other = whole
        .frames()
        .filter(|frame| frame.flag != flagged && frame.flag != unflagged);
    let levels = || whole.frames().map(|frame| frame.level);
    let level = |key, found: Option<u8>| figure(key, "level byte", found.ok_or("no frame"), "");
    [
        Row::new(
            "frames_flagged",
            &with(flagged),
            FRAMES,
            &format!("flag byte {flagged:#04x}"),
        ),
        Row::new(
            "frames_unflagged",
            &with(unflagged),
            FRAMES,
            &format!("flag byte {unflagged:#04x}"),
        ),
        Row::new(
            "frames_other_flag",
            &other.count(),
            FRAMES,
            "any other flag byte",
        ),
        level("level_min", levels().min()),
        level("level_max", levels().max()),
    ]
}

/// The totals of the two counters, a row for each of the first skips with its session and the two values, and the count of those with no row.
fn skip_rows(sessions: &[Session]) -> Vec<Row> {
    let labelled_skips = |skips_of: fn(&Session) -> Vec<Skip>| -> Vec<(usize, Skip)> {
        let per_session = (1_usize..).zip(sessions);
        per_session
            .flat_map(|(ordinal, session)| {
                skips_of(session)
                    .into_iter()
                    .map(move |skip| (ordinal, skip))
            })
            .collect()
    };
    let counters = [
        (
            "packet_counter_skip",
            "a step from one audio notification of a session to the next that is not 1 modulo 256",
            labelled_skips(Session::packet_skips),
        ),
        (
            "frame_counter_skip",
            "a step from one frame of a session to the next that is not 1",
            labelled_skips(Session::frame_skips),
        ),
    ];
    let mut rows = Vec::new();
    for (key, rule, skips) in counters {
        rows.push(Row::new(&format!("{key}s"), &skips.len(), SKIPS, rule));
        rows.extend(skips.iter().take(SKIP_ROWS).map(|(ordinal, skip)| {
            let Skip { before, after } = skip;
            Row::new(
                key,
                &format!("{before} to {after}"),
                "counter",
                &format!("session {ordinal}"),
            )
        }));
        let unlisted = skips.len().saturating_sub(SKIP_ROWS);
        if unlisted > 0 {
            let after = format!("after the first {SKIP_ROWS}, which have a row each");
            rows.push(Row::new(
                &format!("{key}s_unlisted"),
                &unlisted,
                SKIPS,
                &after,
            ));
        }
    }
    rows
}

/// One session and what every row of it reads.
struct Told<'a> {
    /// The first item of every note: which dictation-start mark the row belongs to.
    label: String,
    session: &'a Session,
    heard: Vec<Heard>,
    lead: Option<Lead>,
    /// `skip` when a counter of the session skipped: a lost frame bends every rate, so each of them says so.
    skip: &'static str,
}

/// The rows of one session; `ordinal` counts the dictation-start marks from 1.
pub(super) fn session_rows(ordinal: usize, session: &Session) -> Vec<Row> {
    let heard = session.heard();
    let skipped = !session.packet_skips().is_empty() || !session.frame_skips().is_empty();
    let told = Told {
        label: format!("session {ordinal}"),
        session,
        lead: lead_of(&heard, session.start_ns),
        heard,
        skip: if skipped { "skip" } else { "" },
    };
    let mut rows = told.timing_rows();
    rows.extend(told.lead_rows());
    rows.extend(told.clock_rows());
    rows.extend(told.bitrate_rows());
    rows
}

fn ms(after_ns: u64) -> String {
    format!("{:.3}", millis(after_ns))
}

impl Told<'_> {
    /// Why the session has no lead: too few packets to draw the arrival line from.
    fn no_line() -> String {
        let window_s = seconds(LEAD_WINDOW_NS);
        format!("fewer than {LEAD_MIN_PACKETS} packets with a frame in the first {window_s:.0} s")
    }

    fn timing_rows(&self) -> Vec<Row> {
        let session = self.session;
        let no_reply = labelled(&self.label, &["no reply to the start write in the session"]);
        let no_audio = labelled(&self.label, &["no audio notification in the session"]);
        let stopped = match session.after_stop {
            AfterStop::Unmarked => Err(labelled(
                &self.label,
                &["no dictation-stop mark ends this session"],
            )),
            AfterStop::Unanswered => Err(labelled(
                &self.label,
                &["no reply to the stop write before the next session"],
            )),
            AfterStop::Answered {
                reply_after_ns,
                audio_after_reply,
            } => Ok((reply_after_ns, audio_after_reply)),
        };
        let stopped = stopped.as_ref().map_err(String::as_str);
        let with_tail = "with what session_frames_after_stop counts";
        let after_stop = format!(
            "frames of the session that came after its dictation-stop mark: every audio notification under {TAIL_MS} ms after the mark and before the next dictation-start mark, up to the first that holds no whole frame or holds a frame that does not count one on from the frame before, and none where the session holds no frame before the mark; taken as in flight when the stop was written"
        );
        vec![
            Row::new(
                "session_audio_notifications",
                &session.stream.audio.len(),
                PACKETS,
                &labelled(&self.label, &[with_tail]),
            ),
            Row::new(
                "session_frames",
                &session.stream.frames().count(),
                FRAMES,
                &labelled(&self.label, &[with_tail]),
            ),
            Row::new(
                "session_frames_after_stop",
                &session.tail.taken,
                FRAMES,
                &labelled(&self.label, &[&after_stop]),
            ),
            figure(
                "start_to_reply_ms",
                MS,
                session.reply_after_ns().map(ms).ok_or(&no_reply),
                &labelled(
                    &self.label,
                    &["dictation-start mark to the reply of the pods"],
                ),
            ),
            figure(
                "start_to_audio_ms",
                MS,
                session.audio_after_ns().map(ms).ok_or(&no_audio),
                &labelled(
                    &self.label,
                    &["dictation-start mark to the first audio notification"],
                ),
            ),
            figure(
                "stop_to_reply_ms",
                MS,
                stopped.map(|(reply_after_ns, _)| ms(*reply_after_ns)),
                &labelled(
                    &self.label,
                    &["dictation-stop mark to the reply of the pods"],
                ),
            ),
            figure(
                "audio_after_stop_reply",
                PACKETS,
                stopped.map(|(_, audio_after_reply)| *audio_after_reply),
                &labelled(
                    &self.label,
                    &[
                        "audio notifications after the reply to the stop write, up to the next dictation-start mark",
                    ],
                ),
            ),
        ]
    }

    fn lead_rows(&self) -> [Row; 4] {
        let frame = format!("{FRAME_MS} ms a frame, from the table of contents byte, not measured");
        let said = |what: &str| labelled(&self.label, &[what, &frame, self.skip]);
        let window_s = seconds(LEAD_WINDOW_NS);
        let formula = format!(
            "the least the stream holds from before the dictation-start mark: the mark minus the least, over the packets of the first {window_s:.0} s, of the arrival of a packet minus {FRAME_MS} ms for every frame delivered by the end of it; a frame cannot arrive before its last sample, so the stream began no later than that, and dating a frame by its first sample would give {FRAME_MS} ms less"
        );
        let refused = labelled(&self.label, &[&Self::no_line()]);
        let lead = self.lead.as_ref().ok_or(refused.as_str());
        [
            figure(
                "lead_ms",
                MS,
                lead.map(|lead| format!("{:.3}", lead.lead_ms)),
                &said(&formula),
            ),
            figure(
                "burst_packets",
                PACKETS,
                lead.map(|lead| lead.burst_packets),
                &said(""),
            ),
            figure(
                "burst_frames",
                FRAMES,
                lead.map(|lead| lead.burst_frames),
                &said(""),
            ),
            figure(
                "burst_ms",
                MS,
                lead.map(|lead| format!("{:.3}", lead.burst_ms)),
                &said("first to last packet of the burst"),
            ),
        ]
    }

    /// What `rate` gives of the packets after the burst, or the note of the rows it refuses: no burst is known, or the packets are too thin.
    fn after_burst<T>(&self, rate: fn(&[Heard]) -> Result<T, Thin>) -> Result<T, String> {
        let Some(lead) = &self.lead else {
            let unknown = format!("{}, so the burst is not known", Self::no_line());
            return Err(labelled(&self.label, &[&unknown]));
        };
        rate(after_burst(&self.heard, lead)).map_err(|thin| {
            let held = format!(
                "{} packets after the burst over {:.3} s; needs {} over {:.0} s",
                thin.sdus,
                seconds(thin.span_ns),
                FLOOR.sdus,
                seconds(FLOOR.span_ns)
            );
            labelled(&self.label, &[&held])
        })
    }

    fn clock_rows(&self) -> [Row; 5] {
        let clock = self.after_burst(frame_clock);
        let clock = clock.as_ref().map_err(String::as_str);
        let disagree = clock.is_ok_and(ClockEstimate::disagree);
        let disagree = if disagree { "disagree" } else { "" };
        let assumed = format!("{SAMPLES_PER_FRAME} samples per frame assumed, not measured");
        let scaled =
            |derivation: &str| labelled(&self.label, &[derivation, &assumed, disagree, self.skip]);
        let ratio = clock.map_or(0.0, |clock| clock.ratio);
        let slope = format!(
            "least squares slope over the packets after the burst{NOTE_SEPARATOR}first to last ratio {ratio:.6}"
        );
        [
            figure(
                "frame_rate",
                "frames/s",
                clock.map(|clock| format!("{:.6}", clock.units_per_second)),
                &labelled(&self.label, &[&slope, disagree, self.skip]),
            ),
            figure(
                "clock_hz",
                HZ,
                clock.map(|clock| format!("{:.3}", clock.hz())),
                &scaled(&format!("slope x {SAMPLES_PER_FRAME} samples per frame")),
            ),
            figure(
                "clock_hz_low",
                HZ,
                clock.map(|clock| format!("{:.3}", clock.hz_low())),
                &scaled("low end of the 95 percent interval of the slope"),
            ),
            figure(
                "clock_hz_high",
                HZ,
                clock.map(|clock| format!("{:.3}", clock.hz_high())),
                &scaled("high end of the 95 percent interval of the slope"),
            ),
            figure(
                &format!("clock_ppm_vs_{REFERENCE_HZ}"),
                "ppm",
                clock.map(|clock| format!("{:.1}", clock.ppm_vs(f64::from(REFERENCE_HZ)))),
                &scaled(&format!("clock_hz against {REFERENCE_HZ} Hz")),
            ),
        ]
    }

    fn bitrate_rows(&self) -> [Row; 4] {
        let bits = self.after_burst(bitrate);
        let bits = bits.as_ref().map_err(String::as_str);
        let whole_seconds = bits.map_or(0, |bits| bits.seconds);
        let each = format!(
            "frame bytes x 8 in each whole second after the burst{NOTE_SEPARATOR}{whole_seconds} seconds"
        );
        let each = labelled(&self.label, &[&each, self.skip]);
        let mean = "frame bytes x 8 over the first to last span after the burst";
        [
            figure(
                "bitrate_mean",
                BIT_PER_SECOND,
                bits.map(|bits| format!("{:.1}", bits.mean)),
                &labelled(&self.label, &[mean, self.skip]),
            ),
            figure(
                "bitrate_second_min",
                BIT_PER_SECOND,
                bits.map(|bits| bits.second_min),
                &each,
            ),
            figure(
                "bitrate_second_median",
                BIT_PER_SECOND,
                bits.map(|bits| bits.second_median),
                &each,
            ),
            figure(
                "bitrate_second_max",
                BIT_PER_SECOND,
                bits.map(|bits| bits.second_max),
                &each,
            ),
        ]
    }
}
