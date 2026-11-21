//! The rows a run is reported in: what each holds, when it is refused and what never reaches the table.

mod segment;

use std::fmt::Display;

use pods_proto::battery::{BatteryReport, Charge};
use pods_proto::sdulog::Record;

use super::census::{
    ControlReply, battery, census, control_replies, largest_packet, without_opcode,
};
use super::input::{CutLines, LOG_FILE, MARKS_FILE, Mark, MarkKind};
use super::load::{Loaded, load, windows};

/// The first line of the analysis table.
const TABLE_HEADER: &str = "key\tvalue\tunit\tnote";
/// The first line of the units table.
const UNITS_HEADER: &str = "index\tsdu\tarrival_ns\tstamp\tlen\tmax_sfb";
/// Joins the items of a note.
const NOTE_SEPARATOR: &str = "; ";
/// What a row reads when there is nothing to measure.
const NONE: &str = "none";
const PACKETS: &str = "packets";
const UNITS: &str = "units";
const BYTES: &str = "bytes";
const MS: &str = "ms";
const PERCENT: &str = "percent";

/// One line of the analysis table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    pub(crate) key: String,
    pub(crate) value: String,
    pub(crate) unit: &'static str,
    /// How the value was derived, items joined by `NOTE_SEPARATOR`; may be empty.
    pub(crate) note: String,
}

impl Row {
    fn new(key: &str, value: &dyn Display, unit: &'static str, note: &str) -> Self {
        Self {
            key: key.to_owned(),
            value: value.to_string(),
            unit,
            note: note.to_owned(),
        }
    }
}

/// Everything the analysis of one run writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Report {
    pub(crate) rows: Vec<Row>,
    /// The units table, whole, one line a unit after its header.
    pub(crate) units: String,
    /// The strings of the first device information packet; they can name the device, so no row holds them.
    pub(crate) device_strings: Option<Vec<String>>,
}

impl Report {
    /// The analysis table: a header, then one line a row, each ending in a newline.
    pub(crate) fn table(&self) -> String {
        let mut lines = vec![format!("{TABLE_HEADER}\n")];
        lines.extend(self.rows.iter().map(|row| {
            let Row {
                key,
                value,
                unit,
                note,
            } = row;
            format!("{key}\t{value}\t{unit}\t{note}\n")
        }));
        lines.concat()
    }
}

/// Measures the run: rows for the whole log first, then the rows of each start mark.
pub(crate) fn analyse(records: &[Record], marks: &[Mark], script: &str, cut: CutLines) -> Report {
    let whole = load(records);
    let mut rows = vec![Row::new("script", &script, "name", "from meta.txt")];
    rows.extend(cut_rows(cut));
    rows.extend(count_rows(&whole, marks));
    rows.extend(census_rows(&whole));
    rows.extend(control_rows(&whole, marks));
    rows.extend(battery_rows(&whole));
    rows.push(device_info_row(&whole));
    rows.extend(segment::start_rows(records, marks, &whole));
    Report {
        rows,
        units: units_table(&whole),
        device_strings: whole.device_info.first().cloned(),
    }
}

/// One row for each line file: whether its last line came without a newline and was left out.
fn cut_rows(cut: CutLines) -> [Row; 2] {
    let files = [
        ("sdu_log_cut_lines", LOG_FILE, cut.log),
        ("marks_cut_lines", MARKS_FILE, cut.marks),
    ];
    files.map(|(key, file, cut)| {
        let note = format!(
            "last line of {file} without its newline, left out: a capture killed while writing"
        );
        Row::new(key, &u8::from(cut), "lines", &note)
    })
}

fn count_rows(whole: &Loaded, marks: &[Mark]) -> Vec<Row> {
    let windows = windows(marks);
    let in_a_start = |at_ns| windows.iter().any(|window| window.holds(at_ns));
    let outside = whole.audio.iter().filter(|sdu| !in_a_start(sdu.at_ns));
    let stray = whole.audio.iter().filter(|sdu| sdu.trailing > 0);
    let trailing = whole
        .audio
        .iter()
        .fold(0_usize, |bytes, sdu| bytes.saturating_add(sdu.trailing));
    let trailing_note = format!("after the last whole unit; in {} SDUs", stray.count());
    let cut = marks.iter().filter(|mark| mark.kind == MarkKind::Truncated);
    vec![
        Row::new("packets_from_pods", &whole.seen.len(), PACKETS, ""),
        Row::new(
            "packets_to_pods",
            &whole.to_pods,
            PACKETS,
            "written by the host; in no figure",
        ),
        Row::new(
            "audio_sdus",
            &whole.audio.len(),
            PACKETS,
            "opcode 0x58 subtype 1 from the pods",
        ),
        Row::new(
            "audio_units",
            &whole.units().count(),
            UNITS,
            "whole units in the audio SDUs",
        ),
        Row::new("trailing_bytes", &trailing, BYTES, &trailing_note),
        Row::new(
            "audio_sdus_outside_a_start",
            &outside.count(),
            PACKETS,
            "not between a start mark and the mark that ends it; in no rate; after_stop_audio_sdus has those after the stop mark that ends a start",
        ),
        Row::new(
            "uplink_unknown_subtype",
            &whole.unknown_subtype,
            PACKETS,
            "opcode 0x58 with a subtype that is neither audio nor control",
        ),
        Row::new(
            "uplink_cut_short",
            &whole.cut_short,
            PACKETS,
            "opcode 0x58 too short to read",
        ),
        Row::new(
            "cut_receives",
            &cut.count(),
            PACKETS,
            "receives cut by the buffer; their bytes are not in sdu.log",
        ),
    ]
}

fn census_rows(whole: &Loaded) -> Vec<Row> {
    let largest = match largest_packet(whole) {
        (_, 0) => Row::new(
            "largest_packet",
            &NONE,
            BYTES,
            "no packet from the pods carries an opcode",
        ),
        (opcode, len) => Row::new(
            "largest_packet",
            &len,
            BYTES,
            &format!("opcode 0x{opcode:04x}"),
        ),
    };
    let short = Row::new(
        "packets_without_opcode",
        &without_opcode(whole),
        PACKETS,
        "shorter than 6 bytes",
    );
    let by_opcode = census(whole).into_iter().map(|(opcode, packets)| {
        Row::new(
            &format!("opcode_0x{opcode:04x}"),
            &packets,
            PACKETS,
            "little endian opcode at bytes 4 and 5",
        )
    });
    [largest, short].into_iter().chain(by_opcode).collect()
}

fn control_rows(whole: &Loaded, marks: &[Mark]) -> Vec<Row> {
    let replies = control_replies(whole, marks);
    let count = Row::new(
        "control_packets",
        &replies.len(),
        PACKETS,
        "opcode 0x58 subtype 0 from the pods",
    );
    std::iter::once(count)
        .chain(replies.iter().map(control_row))
        .collect()
}

/// One control packet in full: it holds a subtype, a length and a few payload bytes, nothing that names a device.
fn control_row(reply: &ControlReply) -> Row {
    let hex: Vec<String> = reply
        .bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let place = match reply.nearest {
        Some(nearest) => {
            let kind = match nearest.kind {
                MarkKind::Start => "start",
                MarkKind::Stop => "stop",
                MarkKind::Truncated | MarkKind::Other => "mark",
            };
            let (offset_ms, ordinal) = (nearest.offset_ms, nearest.ordinal);
            format!("{offset_ms:+.3} ms from {kind} {ordinal}")
        }
        None => "no start or stop mark".to_owned(),
    };
    let note = format!(
        "{place}{NOTE_SEPARATOR}length_matches {}",
        reply.length_matches
    );
    Row::new("control_packet", &hex.concat(), "hex", &note)
}

/// Reads the charge of one part out of a battery report.
type ChargeOf = fn(&BatteryReport) -> Option<Charge>;

fn battery_rows(whole: &Loaded) -> Vec<Row> {
    let mut rows = vec![
        Row::new("battery_reports", &whole.batteries.len(), PACKETS, ""),
        Row::new(
            "battery_unread",
            &whole.battery_unread,
            PACKETS,
            "refused by the battery parser",
        ),
    ];
    let Some((first, last)) = battery(whole) else {
        return rows;
    };
    let parts: [(&str, ChargeOf); 4] = [
        ("left", |report| report.batteries.left),
        ("right", |report| report.batteries.right),
        ("case", |report| report.batteries.case),
        ("headset", |report| report.batteries.headset),
    ];
    for (part, charge_of) in parts {
        for (when, report) in [("first", &first), ("last", &last)] {
            let Some(charge) = charge_of(report) else {
                continue;
            };
            let key = format!("battery_{part}_{when}");
            let note = if charge.charging {
                "charging"
            } else {
                "not charging"
            };
            rows.push(Row::new(&key, &charge.level, PERCENT, note));
        }
    }
    rows
}

fn device_info_row(whole: &Loaded) -> Row {
    let packets = whole.device_info.len();
    let note = if packets > 0 {
        "strings of the first are in device-info.txt"
    } else {
        ""
    };
    Row::new("device_info_packets", &packets, PACKETS, note)
}

/// The units table: every whole unit of every audio SDU under the log line it came in.
fn units_table(whole: &Loaded) -> String {
    let mut lines = vec![format!("{UNITS_HEADER}\n")];
    let mut index = 0_usize;
    for sdu in &whole.audio {
        // A log line is counted from 1 and a record from 0.
        let line = sdu.record.saturating_add(1);
        for unit in &sdu.units {
            let max_sfb = unit
                .max_sfb
                .map_or_else(String::new, |limit| limit.to_string());
            let (at_ns, stamp, len) = (sdu.at_ns, unit.stamp, unit.len);
            lines.push(format!(
                "{index}\t{line}\t{at_ns}\t{stamp}\t{len}\t{max_sfb}\n"
            ));
            index = index.saturating_add(1);
        }
    }
    lines.concat()
}

#[cfg(test)]
mod tests;
