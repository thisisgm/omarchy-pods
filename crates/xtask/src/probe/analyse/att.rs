//! Structural analysis of the dictation stream: the audio notifications of the attribute channel, with no audio decoded.

mod load;
mod rows;
mod session;
#[cfg(test)]
mod synth;

use pods_proto::sdulog::Record;

use super::input::Mark;
use super::load::{DICTATION, tails, windows};
use super::report::Row;
// The generator of logs names it as its neighbour, here and in the test of the built command.
#[cfg(test)]
use super::xorshift;
use load::{LoadedAtt, load};
use session::Session;

/// The table of figures of the attribute channel, printed after the analysis table.
pub(crate) const ANALYSIS_FILE: &str = "analysis-att.tsv";
pub(crate) const FRAMES_FILE: &str = "frames.tsv";
/// What the log says of the pods' attribute table; the capture keeps its own list under another name.
pub(crate) const HANDLES_FILE: &str = "att-handles.tsv";
const FRAMES_HEADER: &str = "index\tpacket\tarrival_ns\tsession\tcounter\tflag\tlevel\tlen\ttoc";
const HANDLES_HEADER: &str = "handle\tuuid\tproperties";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Report {
    pub(crate) rows: Vec<Row>,
    pub(crate) frames: String,
    pub(crate) handles: String,
}

/// Measures the attribute log: rows for the whole log first, then the rows of each dictation-start mark; `cut` says whether the last line was left out.
pub(crate) fn analyse(records: &[Record], marks: &[Mark], cut: bool) -> Report {
    let whole = load(records);
    let windows = windows(marks, DICTATION);
    let cuts = windows.iter().zip(tails(marks, DICTATION));
    let sessions: Vec<Session> = cuts
        .map(|(window, tail)| Session::cut(records, *window, tail))
        .collect();
    let session_of = |record: usize| {
        let holds = |session: &Session| session.records.contains(&record);
        sessions.iter().position(holds)
    };
    let outside = whole
        .audio
        .iter()
        .filter(|packet| session_of(packet.record).is_none());
    let mut rows = rows::log_rows(&whole, &sessions, outside.count(), cut);
    for (ordinal, session) in (1_usize..).zip(&sessions) {
        rows.extend(rows::session_rows(ordinal, session));
    }
    Report {
        rows,
        frames: frames_table(&whole, session_of),
        handles: handles_table(&whole),
    }
}

/// The frames table: every whole frame under the log line it came in and the session that takes that line, counted from 1 and empty outside one.
fn frames_table(whole: &LoadedAtt, session_of: impl Fn(usize) -> Option<usize>) -> String {
    let mut lines = vec![format!("{FRAMES_HEADER}\n")];
    let mut index = 0_usize;
    for packet in &whole.audio {
        // A log line is counted from 1 and a record from 0, as a session is and a window is.
        let line = packet.record.saturating_add(1);
        let session = session_of(packet.record)
            .map_or_else(String::new, |session| session.saturating_add(1).to_string());
        for frame in &packet.frames {
            let toc = frame
                .toc
                .map_or_else(String::new, |toc| format!("{toc:#04x}"));
            let (at_ns, counter, flag, level, len) = (
                packet.at_ns,
                frame.counter,
                frame.flag,
                frame.level,
                frame.len,
            );
            lines.push(format!(
                "{index}\t{line}\t{at_ns}\t{session}\t{counter}\t{flag:#04x}\t{level}\t{len}\t{toc}\n"
            ));
            index = index.saturating_add(1);
        }
    }
    lines.concat()
}

fn handles_table(whole: &LoadedAtt) -> String {
    let mut lines = vec![format!("{HANDLES_HEADER}\n")];
    lines.extend(whole.handles.iter().map(|(handle, listed)| {
        let properties = listed
            .properties
            .map_or_else(String::new, |properties| format!("{properties:#04x}"));
        format!("{handle:#06x}\t{}\t{properties}\n", listed.uuid)
    }));
    lines.concat()
}

#[cfg(test)]
mod tests;
