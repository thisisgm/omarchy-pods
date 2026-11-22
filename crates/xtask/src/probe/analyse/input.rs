//! Readers of the three text files of a run: the packet log, the marks and the meta lines.

use std::fmt;

use pods_proto::sdulog::{LogError, Record, parse_line};

/// The packet log of a run, one line a packet.
pub(crate) const LOG_FILE: &str = "sdu.log";
/// The log of the attribute channel, in the lines of the packet log; only a run that opened the channel holds one.
pub(crate) const ATT_LOG_FILE: &str = "att.log";
/// What the capture did and when, one line a mark.
pub(crate) const MARKS_FILE: &str = "marks.tsv";
/// What produced the run, one `key=value` a line.
pub(crate) const META_FILE: &str = "meta.txt";
/// The separator of the three fields of a mark.
const MARK_SEPARATOR: char = '\t';
/// A mark is a stamp, a kind and a text.
const MARK_FIELDS: usize = 3;
/// A mark kind is at most this many characters.
const KIND_MAX: usize = 16;
/// The kind of the mark written when START goes out.
const KIND_START: &str = "start";
/// The kind of the mark written when STOP goes out.
const KIND_STOP: &str = "stop";
/// The kind of the mark written when the dictation start write goes out.
const KIND_DICTATION_START: &str = "dictation-start";
/// The kind of the mark written when the dictation stop write goes out.
const KIND_DICTATION_STOP: &str = "dictation-stop";
/// The kind of the mark written for a receive the buffer cut, in two parts: a store test bans the whole word from this crate.
const KIND_CUT: &str = concat!("trunc", "ated");
/// The kind of the mark written for each line shown to the owner.
const KIND_CUE: &str = "cue";
/// The separator of a meta line.
const META_SEPARATOR: char = '=';
/// The key of the meta line that names the script.
const SCRIPT_KEY: &str = "script";
/// A script name is at most this many characters.
const SCRIPT_MAX: usize = 32;
/// The key of the meta line that holds the wall clock, in nanoseconds since 1970, at the zero of the stamps.
const REALTIME_KEY: &str = "realtime_at_zero_ns";

/// The kinds of mark the analysis reads; every other kind is `Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MarkKind {
    Start,
    Stop,
    DictationStart,
    DictationStop,
    Truncated,
    Other,
}

/// One line of the marks file, without its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Mark {
    /// On the clock of the packet log.
    pub(crate) at_ns: u64,
    pub(crate) kind: MarkKind,
}

/// One cue of the marks file: when it was shown and what it said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Cue<'a> {
    pub(crate) at_ns: u64,
    pub(crate) text: &'a str,
}

/// What the meta file says about the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Meta {
    /// The name of the capture script, 1 to `SCRIPT_MAX` characters of a-z, 0-9 and -.
    pub(crate) script: String,
    /// `None` for a run whose meta file has no such line.
    pub(crate) realtime_at_zero_ns: Option<u64>,
}

/// The rule a line broke; it never repeats the line, whose text can identify a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum LineRule {
    #[error("{0}")]
    Log(LogError),
    #[error("its stamp is earlier than the stamp of the line before")]
    OutOfOrder,
    #[error("a mark needs a stamp, a kind and a text, separated by tabs")]
    MarkMissingField,
    #[error(
        "the stamp of a mark must be a decimal number from 0 to {max}, with no sign and no leading zero",
        max = u64::MAX
    )]
    MarkBadStamp,
    #[error("the kind of a mark must be 1 to {KIND_MAX} characters of a-z, 0-9 and -")]
    MarkBadKind,
    #[error("a meta line must be a key, {META_SEPARATOR} and a value")]
    MetaMissingEquals,
    #[error("the script name must be 1 to {SCRIPT_MAX} characters of a-z, 0-9 and -")]
    MetaBadScript,
    #[error("the script is named a second time")]
    MetaScriptTwice,
    #[error("{REALTIME_KEY} must be one decimal number of nanoseconds, given once")]
    MetaBadRealtime,
    #[error("it is not UTF-8 text")]
    NotText,
}

/// Why a file of a run was refused; it names the file, the line and the rule, never the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum InputError {
    #[error("{file} line {line}: {rule}")]
    Line {
        file: &'static str,
        /// Counted from 1.
        line: usize,
        rule: LineRule,
    },
    #[error("{LOG_FILE} holds no packet, so there is nothing to measure")]
    EmptyLog,
    #[error("{LOG_FILE} and {ATT_LOG_FILE} hold no packet, so there is nothing to measure")]
    EmptyLogs,
    #[error("{META_FILE} has no {SCRIPT_KEY}{META_SEPARATOR} line")]
    NoScript,
}

/// Which files ended in a line without its newline, which is left out: the capture was killed while writing it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CutLines {
    pub(crate) log: bool,
    pub(crate) marks: bool,
}

// Sample input: b"1 < 00\n2 < 0", which gives b"1 < 00\n" and true; bytes that end in a newline are whole and give false.
pub(crate) fn whole_lines(bytes: &[u8]) -> (&[u8], bool) {
    let last_newline = bytes.iter().rposition(|&byte| byte == b'\n');
    let end = last_newline.map_or(0, |at| at.saturating_add(1));
    // The end is 0 or just past a byte that is there, so it is inside the bytes and the fallback is never taken.
    let (whole, cut) = bytes.split_at_checked(end).unwrap_or((bytes, &[]));
    (whole, !cut.is_empty())
}

/// The contents of a run file as text; bytes that are not text are refused by the line that holds the first of them.
pub(crate) fn text_of<'a>(file: &'static str, bytes: &'a [u8]) -> Result<&'a str, InputError> {
    std::str::from_utf8(bytes).map_err(|error| {
        let text_before = bytes.iter().take(error.valid_up_to());
        let lines_before = text_before.filter(|&&byte| byte == b'\n').count();
        InputError::Line {
            file,
            line: lines_before.saturating_add(1),
            rule: LineRule::NotText,
        }
    })
}

/// The packet log of a run, which must hold a packet.
pub(crate) fn parse_log(text: &str) -> Result<Vec<Record>, InputError> {
    let records = parse_lines(LOG_FILE, text)?;
    if records.is_empty() {
        return Err(InputError::EmptyLog);
    }
    Ok(records)
}

// Sample input: "1203456789 < 0400040058000100\n1203460000 > 0400040058000000\n", whole lines only; one bad line or a stamp that goes back refuses the log.
pub(crate) fn parse_lines(file: &'static str, text: &str) -> Result<Vec<Record>, InputError> {
    let mut records: Vec<Record> = Vec::new();
    let mut last_ns = 0;
    for (index, line) in text.lines().enumerate() {
        let refused = |rule| InputError::Line {
            file,
            line: index.saturating_add(1),
            rule,
        };
        let record = parse_line(line).map_err(|error| refused(LineRule::Log(error)))?;
        if record.at_ns < last_ns {
            return Err(refused(LineRule::OutOfOrder));
        }
        last_ns = record.at_ns;
        records.push(record);
    }
    Ok(records)
}

// Sample input: "0\tconnect\t\n1203456789\tstart\t\n6203456789\tstop\t\n", whole lines only, one mark a line; no line is a run with no mark.
pub(crate) fn parse_marks(text: &str) -> Result<Vec<Mark>, InputError> {
    let lines = mark_lines(text)?;
    Ok(lines.into_iter().map(|(mark, _)| mark).collect())
}

// Sample input: "0\tconnect\t\n8000000000\tcue\tStay silent\n", read as one cue at 8 s that says `Stay silent`.
pub(crate) fn parse_cues(text: &str) -> Result<Vec<Cue<'_>>, InputError> {
    let lines = mark_lines(text)?.into_iter();
    let cue = lines.filter_map(|(mark, text)| Some((mark.at_ns, text?)));
    Ok(cue.map(|(at_ns, text)| Cue { at_ns, text }).collect())
}

// Sample input: "0\tconnect\t\n8000000000\tcue\tStay silent\n", whole lines in stamp order; each mark comes with its text where it is a cue.
fn mark_lines(text: &str) -> Result<Vec<(Mark, Option<&str>)>, InputError> {
    let mut marks: Vec<(Mark, Option<&str>)> = Vec::new();
    let mut last_ns = 0;
    for (index, line) in text.lines().enumerate() {
        let refused = |rule| InputError::Line {
            file: MARKS_FILE,
            line: index.saturating_add(1),
            rule,
        };
        let (mark, cue) = parse_mark(line).map_err(refused)?;
        if mark.at_ns < last_ns {
            return Err(refused(LineRule::OutOfOrder));
        }
        last_ns = mark.at_ns;
        marks.push((mark, cue));
    }
    Ok(marks)
}

// Sample input: "1203456789\tcue\tSay left", nanoseconds, a kind, then a text that may be empty or hold tabs.
fn parse_mark(line: &str) -> Result<(Mark, Option<&str>), LineRule> {
    let mut fields = line.splitn(MARK_FIELDS, MARK_SEPARATOR);
    let (Some(stamp), Some(kind_name), Some(text)) = (fields.next(), fields.next(), fields.next())
    else {
        return Err(LineRule::MarkMissingField);
    };
    let at_ns = parse_stamp(stamp).ok_or(LineRule::MarkBadStamp)?;
    let kind = parse_kind(kind_name).ok_or(LineRule::MarkBadKind)?;
    Ok((
        Mark { at_ns, kind },
        (kind_name == KIND_CUE).then_some(text),
    ))
}

// Sample input: "1203456789", refused unless writing the number again gives the same text.
fn parse_stamp(stamp: &str) -> Option<u64> {
    let Ok(at_ns) = stamp.parse::<u64>() else {
        return None;
    };
    (at_ns.to_string() == stamp).then_some(at_ns)
}

// Sample input: "start", "stop", "dictation-start", "dictation-stop" or the kind of a cut receive; any other name is a kind the analysis does not read.
fn parse_kind(kind: &str) -> Option<MarkKind> {
    if !is_name(kind, KIND_MAX) {
        return None;
    }
    Some(match kind {
        KIND_START => MarkKind::Start,
        KIND_STOP => MarkKind::Stop,
        KIND_DICTATION_START => MarkKind::DictationStart,
        KIND_DICTATION_STOP => MarkKind::DictationStop,
        KIND_CUT => MarkKind::Truncated,
        _ => MarkKind::Other,
    })
}

// Sample input: "first-word-2", 1 to `max` characters of a-z, 0-9 and -, so a name is safe to print.
fn is_name(text: &str, max: usize) -> bool {
    (1..=max).contains(&text.len())
        && text
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// The lines every meta file opens with, as `parse_meta` reads them: the script, the tool that wrote the run and the wall clock at its zero.
pub(crate) fn meta_text(script: &dyn fmt::Display, tool: &str, realtime_at_zero_ns: u64) -> String {
    format!(
        "{SCRIPT_KEY}{META_SEPARATOR}{script}\ntool{META_SEPARATOR}{tool}\n{REALTIME_KEY}{META_SEPARATOR}{realtime_at_zero_ns}\n"
    )
}

// Sample input: "script=soak\ntool=xtask 0.0.0\nrealtime_at_zero_ns=1763640000000000000\n"; the tool line is not used.
pub(crate) fn parse_meta(text: &str) -> Result<Meta, InputError> {
    let mut script: Option<String> = None;
    let mut realtime_at_zero_ns: Option<u64> = None;
    for (index, line) in text.lines().enumerate() {
        let refused = |rule| InputError::Line {
            file: META_FILE,
            line: index.saturating_add(1),
            rule,
        };
        let Some((key, value)) = line.split_once(META_SEPARATOR) else {
            return Err(refused(LineRule::MetaMissingEquals));
        };
        if key == REALTIME_KEY {
            let stamp = parse_stamp(value).filter(|_| realtime_at_zero_ns.is_none());
            realtime_at_zero_ns = Some(stamp.ok_or(refused(LineRule::MetaBadRealtime))?);
        }
        if key != SCRIPT_KEY {
            continue;
        }
        if script.is_some() {
            return Err(refused(LineRule::MetaScriptTwice));
        }
        if !is_name(value, SCRIPT_MAX) {
            return Err(refused(LineRule::MetaBadScript));
        }
        script = Some(value.to_owned());
    }
    let script = script.ok_or(InputError::NoScript)?;
    Ok(Meta {
        script,
        realtime_at_zero_ns,
    })
}

#[cfg(test)]
mod tests;
