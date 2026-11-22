//! Dictation runs for the tests of `probe analyse`: the marks of generated sessions, and what the command wrote about the attribute channel of a run.

use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use super::att_synth::{NS_PER_SECOND, Session};
use super::support::{make_run_of, probe, row};

/// Where the first session of every generated run starts.
pub(crate) const START_NS: u64 = 2 * NS_PER_SECOND;
/// The header of the frames table.
pub(crate) const FRAMES_HEADER: &str =
    "index\tpacket\tarrival_ns\tsession\tcounter\tflag\tlevel\tlen\ttoc";
/// One packet on the control channel, so the packet log of every run here holds something.
pub(crate) const CONTROL_LOG: &str = "1 < 01\n";

/// What the command wrote about the attribute channel of one run.
pub(crate) struct Analysed {
    pub(crate) run: PathBuf,
    /// The text of `analysis-att.tsv`.
    pub(crate) table: String,
    /// The text of `frames.tsv`.
    pub(crate) frames: String,
}

impl Analysed {
    /// The value and the note of every row with this key, in order.
    // Sample input: `frame_rate\t50.000000\tframes/s\tsession 1; least squares slope`, a row's key, value, unit and note.
    pub(crate) fn rows(&self, key: &str) -> Vec<(&str, &str)> {
        let rows = self.table.lines().filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            (fields[0] == key).then(|| (fields[1], fields[3]))
        });
        rows.collect()
    }

    pub(crate) fn value(&self, key: &str) -> &str {
        row(&self.table, key).0
    }

    pub(crate) fn number(&self, key: &str) -> f64 {
        self.value(key).parse().unwrap()
    }
}

// Sample output: `2000000000\tdictation-start\tdictation start`, then the stop mark of the same session.
pub(crate) fn marks_of(sessions: &[Session]) -> String {
    let mut marks = vec!["0\tconnect\tchannel open\n".to_owned()];
    for session in sessions {
        let (start_ns, stop_ns) = (session.start_ns, session.stop_ns);
        marks.push(format!("{start_ns}\tdictation-start\tdictation start\n"));
        marks.push(format!("{stop_ns}\tdictation-stop\tdictation stop\n"));
    }
    marks.concat()
}

/// Runs the command on a new run that holds `att_log` and `marks`, and reads what it wrote.
pub(crate) fn analyse(home: &TempDir, name: &str, att_log: &str, marks: &str) -> Analysed {
    let files = [
        ("sdu.log", CONTROL_LOG),
        ("att.log", att_log),
        ("marks.tsv", marks),
    ];
    let run = make_run_of(home, name, &files);
    let done = probe(home, "analyse", &run).output().unwrap();
    assert!(done.status.success(), "{done:?}");
    assert_eq!(String::from_utf8(done.stderr).unwrap(), "");
    let table = fs::read_to_string(run.join("analysis-att.tsv")).unwrap();
    let uplink = fs::read_to_string(run.join("analysis.tsv")).unwrap();
    assert_eq!(String::from_utf8(done.stdout).unwrap(), uplink + &table);
    let frames = fs::read_to_string(run.join("frames.tsv")).unwrap();
    Analysed { run, table, frames }
}
