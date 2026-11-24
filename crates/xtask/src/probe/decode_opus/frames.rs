//! The frames of the dictation stream as the attribute log holds them, each under the session the frames table puts it in.

use pods_proto::dictation::{DictationPacket, Frame};
use pods_proto::doap::Handles;
use pods_proto::sdulog::{Direction, Record};

use crate::probe::analyse::att::FRAMES_FILE;
use crate::probe::analyse::input::ATT_LOG_FILE;

/// The fields of one line of the frames table, and which of them holds the session, counted from 0.
const COLUMNS: usize = 9;
const SESSION_COLUMN: usize = 3;
/// The first line of the frames table is its header.
const FIRST_FRAME_LINE: usize = 2;

/// One whole frame of an audio notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Logged<'log> {
    /// The session of the frames table, counted from 1; none outside every session.
    pub(crate) session: Option<u32>,
    pub(crate) flag: u8,
    pub(crate) level: u8,
    /// The coded bytes; none for a frame that does not open with the table of contents byte of the stream, which nobody decodes.
    pub(crate) coded: Option<&'log [u8]>,
}

/// Why the frames of a run were not loaded; it never repeats a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum FramesError {
    #[error(
        "line {line} of {FRAMES_FILE} is not {COLUMNS} fields with a session number or nothing in the fourth"
    )]
    Line { line: usize },
    #[error(
        "{FRAMES_FILE} lists {listed} frames and {ATT_LOG_FILE} holds {held}, so the two are not of one run"
    )]
    Count { listed: usize, held: usize },
}

// Sample input: `7\t3\t2080000000\t1\t7\t0x80\t213\t81\t0xb8`, a frame of session 1; outside a session the fourth field is empty.
fn session_of(line: usize, text: &str) -> Result<Option<u32>, FramesError> {
    let fields: Vec<&str> = text.split('\t').collect();
    match fields.get(SESSION_COLUMN) {
        Some(&"") if fields.len() == COLUMNS => Ok(None),
        Some(number) if fields.len() == COLUMNS => {
            let session = number
                .parse()
                .map_err(|_not_a_number| FramesError::Line { line })?;
            Ok(Some(session))
        }
        _ => Err(FramesError::Line { line }),
    }
}

// Sample input: a header line that is skipped unread, then `7\t3\t2080000000\t1\t7\t0x80\t213\t81\t0xb8` for every frame; gives the session of each, in order.
pub(crate) fn sessions(table: &str) -> Result<Vec<Option<u32>>, FramesError> {
    let lines = (FIRST_FRAME_LINE..).zip(table.lines().skip(1));
    lines.map(|(line, text)| session_of(line, text)).collect()
}

/// Every whole frame of every audio notification the pods sent, in log order, which is the order of the frames table.
pub(crate) fn load<'log>(
    records: &'log [Record],
    table: &str,
) -> Result<Vec<Logged<'log>>, FramesError> {
    let sessions = sessions(table)?;
    let from_pods = records
        .iter()
        .filter(|record| record.direction == Direction::FromPods);
    let held: Vec<Frame<'log>> = from_pods
        .filter_map(
            |record| match DictationPacket::parse(&record.sdu, &Handles::OBSERVED) {
                Ok(Some(DictationPacket::Audio(audio))) => Some(audio.frames()),
                _ => None,
            },
        )
        .flatten()
        .collect();
    if held.len() != sessions.len() {
        return Err(FramesError::Count {
            listed: sessions.len(),
            held: held.len(),
        });
    }
    let logged = held
        .into_iter()
        .zip(sessions)
        .map(|(frame, session)| Logged {
            session,
            flag: frame.flag,
            level: frame.level,
            coded: frame.is_expected_codec().then_some(frame.data),
        });
    Ok(logged.collect())
}
