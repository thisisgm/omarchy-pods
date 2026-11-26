//! Superwhisper's recorders: stock `pw-record` children with its arguments, ended with SIGINT as it ends them.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use rustix::process::Signal;

use crate::wav::{self, BYTES_PER_SAMPLE, HEADER_LEN};
use crate::{Process, Server, read_at_most, wait_until};

/// The node and the rate of Superwhisper's recorder.
const CAPTURE: (&str, u32) = ("superwhisper-capture", 16_000);
/// The node and the rate of the copy Superwhisper records when it keeps recordings.
const HISTORY: (&str, u32) = ("superwhisper-history", 48_000);

/// Superwhisper records mono.
const CHANNELS: u16 = 1;

/// How long a recorder may take to close its file after SIGINT.
const FINISH_WITHIN: Duration = Duration::from_secs(5);
/// pw-record writes only while its source plays (a 4 s tone left 4.05 s over 12 s); the longest recording a test makes is ten minutes.
pub const LONGEST_RECORDING: Duration = Duration::from_mins(11);

#[derive(Debug)]
pub struct Recorder<'server> {
    process: Process<'server>,
    file: PathBuf,
    node: &'static str,
    rate: u32,
}

impl<'server> Recorder<'server> {
    /// Superwhisper's recorder, 16 kHz mono 16 bit into `file`.
    pub fn superwhisper(server: &'server Server, target: &str, file: &Path) -> Self {
        Self::start(server, target, file, CAPTURE)
    }

    /// Superwhisper's second recorder, 48 kHz mono 16 bit into `file`.
    pub fn history(server: &'server Server, target: &str, file: &Path) -> Self {
        Self::start(server, target, file, HISTORY)
    }

    /// A regular desktop recorder at the same format as the history recorder.
    pub fn program(server: &'server Server, target: &str, file: &Path) -> Self {
        Self::start(server, target, file, ("test-recorder", HISTORY.1))
    }

    /// Records the stock player at the history rate beside both daemon recorders.
    pub fn control(server: &'server Server, target: &str, file: &Path) -> Self {
        Self::start(server, target, file, ("stock-player-control", HISTORY.1))
    }

    fn start(
        server: &'server Server,
        target: &str,
        file: &Path,
        (node, rate): (&'static str, u32),
    ) -> Self {
        let mut command = server.command("pw-record");
        // Superwhisper's own arguments, latency and resampler quality included, copied as it passes them.
        command
            .args(["--latency", "20ms", "--rate", &rate.to_string()])
            .args(["--channels", &CHANNELS.to_string()])
            .args(["--format", "s16", "--quality", "10"])
            .args(["--target", target])
            .args(["--properties", &format!("node.name={node}")])
            .arg(file);
        Self {
            process: Process::start(&mut command, Stdio::null(), file.with_extension("log")),
            file: file.to_owned(),
            node,
            rate,
        }
    }

    /// The node name the recorder gives itself, which `Server::link` takes.
    #[must_use]
    pub fn node(&self) -> &'static str {
        self.node
    }

    /// The rate the recorder asks for, which its samples are at.
    #[must_use]
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Waits for the file to hold `frames` frames, failing once `within` passes.
    pub fn wait_frames(&self, frames: usize, within: Duration) {
        let wanted = u64::try_from(frames).unwrap_or(u64::MAX);
        let mut held = 0;
        let reached = wait_until(within, || {
            held = self.frames_on_disk();
            (held >= wanted).then_some(())
        });
        reached.unwrap_or_else(|| {
            panic!(
                "{} holds {held} of {frames} frames after {within:?}; its log: {}",
                self.node,
                self.process.log()
            )
        });
    }

    /// The frames in the file, from its length alone, since pw-record writes a header of a fixed length.
    #[must_use]
    pub fn frames_on_disk(&self) -> u64 {
        let length = match fs::metadata(&self.file) {
            Ok(metadata) => metadata.len(),
            // pw-record makes its file before its node appears, so only a caller that did not wait for the node sees none.
            Err(error) if error.kind() == ErrorKind::NotFound => 0,
            Err(error) => panic!("{}: {error}", self.file.display()),
        };
        // A file shorter than its header is one pw-record is still starting; the divisor is a nonzero constant.
        length
            .saturating_sub(u64::from(HEADER_LEN))
            .checked_div(u64::from(BYTES_PER_SAMPLE))
            .unwrap_or(0)
    }

    /// Sends SIGINT, waits for the exit and gives the samples of the file the recorder closed.
    #[must_use]
    pub fn finish(mut self) -> Vec<i16> {
        let status = self.process.signal(Signal::INT, FINISH_WITHIN);
        // pw-record exits with 1 after SIGINT, so only an end by a signal is a failure.
        assert!(
            status.code().is_some(),
            "{} ended by {status}; its log: {}",
            self.node,
            self.process.log()
        );
        let bytes = read_at_most(&self.file, recording_limit())
            .unwrap_or_else(|reason| panic!("{}: {reason}", self.node));
        let wav = wav::parse(&bytes)
            .unwrap_or_else(|| panic!("{} is not a 16 bit PCM WAV file", self.file.display()));
        assert_eq!(
            (wav.rate, wav.channels),
            (self.rate, CHANNELS),
            "the rate and channels of {}",
            self.file.display()
        );
        assert!(
            u64::try_from(wav.samples_at).is_ok_and(|at| at == u64::from(HEADER_LEN)),
            "{}: the first sample starts at byte {}, and `wait_frames` counts from byte {HEADER_LEN}",
            self.file.display(),
            wav.samples_at
        );
        assert!(
            wav.closed(),
            "the header of {} does not count its data, so the recorder never closed it",
            self.file.display()
        );
        wav.samples
    }
}

/// The most `finish` reads: the longest recording at the history rate, plus the header.
fn recording_limit() -> u64 {
    LONGEST_RECORDING
        .as_secs()
        .saturating_mul(u64::from(HISTORY.1))
        .saturating_mul(u64::from(BYTES_PER_SAMPLE))
        .saturating_add(u64::from(HEADER_LEN))
}
