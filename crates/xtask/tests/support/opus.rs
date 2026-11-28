//! Dictation runs of chosen frames for the tests of `probe decode-opus`, the command on them, and what it wrote.

use std::env;
use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

use pods_proto::dictation::{Frame, STARTED, STOPPED, TOC_CELT_WIDEBAND_20MS_MONO, encode_audio};
use pods_proto::doap::{self, Handles};
use pods_proto::sdulog::{Direction, Record};
use tempfile::TempDir;

use super::att_run::{Analysed, analyse, marks_of};
use super::att_synth::{NS_PER_MS, NS_PER_SECOND, Session};
use super::support::{log_text, make_run_of, probe, row};
use super::xorshift::next_random;

#[expect(
    dead_code,
    reason = "what only the tests of the whole measure read of the dictation audio goes unused here"
)]
#[path = "dictation.rs"]
mod dictation;
pub(crate) use dictation::{FRAME, SAMPLE_BYTES};

/// The status the command exits with when it refuses; a panic would exit with 101.
pub(crate) const REFUSED: i32 = 1;
/// The bytes before the samples in the WAV file the command writes.
const WAV_HEADER_BYTES: usize = 44;
/// A float sample times this is its value in steps of a 16 bit sample.
const LSB_PER_FULL_SCALE: f64 = 32768.0;
/// The coded bytes after the table of contents byte of a seeded frame.
const SEEDED_BYTES: usize = 80;
/// The lowest level byte of a seeded frame and how many values above it are drawn.
const LEVELS: (u8, u64) = (185, 38);
/// Where the first session starts, how long a frame lasts, and the idle time between two sessions.
const START_NS: u64 = 2 * NS_PER_SECOND;
const FRAME_NS: u64 = 20 * NS_PER_MS;
const HALF_FRAME_NS: u64 = FRAME_NS / 2;
const BETWEEN_NS: u64 = 3 * NS_PER_SECOND;
/// The mode of a stand-in for FFmpeg: its owner may run it.
const RUNNABLE: u32 = 0o700;

pub(crate) fn count(len: usize) -> f64 {
    f64::from(u32::try_from(len).unwrap())
}

pub(crate) struct Sent {
    pub(crate) flag: u8,
    pub(crate) level: u8,
    pub(crate) data: Vec<u8>,
}

/// Frames of the table of contents byte alone.
pub(crate) fn silent(frames: usize) -> Vec<Sent> {
    let alone = || Sent {
        flag: 0,
        level: LEVELS.0,
        data: vec![TOC_CELT_WIDEBAND_20MS_MONO],
    };
    (0..frames).map(|_| alone()).collect()
}

/// Frames of the table of contents byte and 80 bytes from the generator, each with a flag and a level drawn from it too.
pub(crate) fn seeded(seed: u64, frames: usize) -> Vec<Sent> {
    let mut random = seed;
    let mut frame = || {
        let mut data = vec![TOC_CELT_WIDEBAND_20MS_MONO];
        data.extend((0..SEEDED_BYTES).map(|_| next_random(&mut random).to_le_bytes()[0]));
        let flag = if next_random(&mut random).is_multiple_of(2) {
            0x80
        } else {
            0x00
        };
        let above = next_random(&mut random).checked_rem(LEVELS.1).unwrap();
        Sent {
            flag,
            level: LEVELS.0.saturating_add(u8::try_from(above).unwrap()),
            data,
        }
    };
    (0..frames).map(|_| frame()).collect()
}

/// One session from `start_ns`: the start write and its reply, each frame in a notification of its own 20 ms after the one before, the stop write and its reply.
fn session(start_ns: u64, frames: &[Sent]) -> Session {
    let record = |at_ns, direction, sdu: &[u8]| Record {
        at_ns,
        direction,
        sdu: sdu.to_vec(),
    };
    let mut at_ns = start_ns.saturating_add(FRAME_NS);
    let mut records = vec![
        record(
            start_ns,
            Direction::ToPods,
            &doap::start(&Handles::OBSERVED),
        ),
        record(at_ns, Direction::FromPods, &STARTED),
    ];
    for (counter, frame) in (0_u16..).zip(frames) {
        at_ns = at_ns.saturating_add(FRAME_NS);
        let coded = Frame {
            flag: frame.flag,
            level: frame.level,
            counter,
            data: &frame.data,
        };
        let packet = counter.to_le_bytes()[0];
        let notification = encode_audio(&Handles::OBSERVED, packet, &[coded]).unwrap();
        records.push(record(at_ns, Direction::FromPods, &notification));
    }
    let stop_ns = at_ns.saturating_add(HALF_FRAME_NS);
    let stop = doap::stop(&Handles::OBSERVED, doap::STOP_REASON_HOST);
    records.push(record(stop_ns, Direction::ToPods, &stop));
    let stopped_ns = stop_ns.saturating_add(FRAME_NS);
    records.push(record(stopped_ns, Direction::FromPods, &STOPPED));
    Session {
        records,
        start_ns,
        stop_ns,
        next_packet: 0,
    }
}

/// A run of one session for each list of frames, analysed by the built command, in a new temporary home.
pub(crate) fn analysed(name: &str, sessions: &[Vec<Sent>]) -> (TempDir, Analysed) {
    let home = tempfile::tempdir().unwrap();
    let mut start_ns = START_NS;
    let mut made = Vec::new();
    for frames in sessions {
        let session = session(start_ns, frames);
        start_ns = session.stop_ns.saturating_add(BETWEEN_NS);
        made.push(session);
    }
    let records: Vec<Record> = made
        .iter()
        .flat_map(|session| session.records.clone())
        .collect();
    let analysed = analyse(&home, name, &log_text(&records), &marks_of(&made));
    (home, analysed)
}

/// A new run that holds the attribute log of `of` and `table` as its frames table.
pub(crate) fn with_frames_table(home: &TempDir, name: &str, of: &Analysed, table: &str) -> PathBuf {
    let log = fs::read_to_string(of.run.join("att.log")).unwrap();
    make_run_of(home, name, &[("att.log", &log), ("frames.tsv", table)])
}

/// A new run that holds the attribute log of `of` and was never analysed.
pub(crate) fn without_frames_table(home: &TempDir, name: &str, of: &Analysed) -> PathBuf {
    let log = fs::read_to_string(of.run.join("att.log")).unwrap();
    make_run_of(home, name, &[("att.log", &log)])
}

/// A directory that holds one program named ffmpeg: a shell script that adds its arguments to the file `asked` beside it, one a line and an empty line after them, writes `frames` frames of zero samples, then does `then`.
pub(crate) fn stand_in(frames: usize, then: &str) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("ffmpeg");
    let bytes = frames.saturating_mul(FRAME * SAMPLE_BYTES);
    let asked = dir.path().join("asked");
    let asked = asked.display();
    let text = format!(
        "#!/bin/sh\n{{ printf '%s\\n' \"$@\"; echo; }} >> '{asked}'\nhead -c {bytes} /dev/zero\n{then}\n"
    );
    fs::write(&script, text).unwrap();
    fs::set_permissions(&script, Permissions::from_mode(RUNNABLE)).unwrap();
    dir
}

/// The command on `run`, with the program of `oracle` found before any FFmpeg of the machine when one is given.
pub(crate) fn decode(home: &TempDir, run: &Path, oracle: Option<&TempDir>) -> Output {
    let mut command = probe(home, "decode-opus", run);
    if let Some(oracle) = oracle {
        let rest = env::var_os("PATH").unwrap();
        let first = [oracle.path().to_path_buf()].into_iter();
        let path = env::join_paths(first.chain(env::split_paths(&rest))).unwrap();
        command.env("PATH", path);
    }
    command.output().unwrap()
}

/// A run the command decoded, and the table it printed and left in the run.
pub(crate) struct Decoded {
    pub(crate) home: TempDir,
    pub(crate) run: PathBuf,
    pub(crate) table: String,
}

/// A run of one session for each list of frames, analysed and then decoded with the FFmpeg of the machine.
pub(crate) fn decoded(name: &str, sessions: &[Vec<Sent>]) -> Decoded {
    Decoded::through(None, name, sessions)
}

impl Decoded {
    /// The same with the program of `oracle` standing in for FFmpeg.
    pub(crate) fn with(oracle: &TempDir, name: &str, sessions: &[Vec<Sent>]) -> Self {
        Self::through(Some(oracle), name, sessions)
    }

    fn through(oracle: Option<&TempDir>, name: &str, sessions: &[Vec<Sent>]) -> Self {
        let (home, analysed) = analysed(name, sessions);
        let run = analysed.run;
        let done = decode(&home, &run, oracle);
        assert!(done.status.success(), "{done:?}");
        assert_eq!(String::from_utf8(done.stderr).unwrap(), "");
        let table = String::from_utf8(done.stdout).unwrap();
        assert_eq!(
            fs::read_to_string(run.join("decode-opus.tsv")).unwrap(),
            table
        );
        Self { home, run, table }
    }

    pub(crate) fn value(&self, key: &str) -> &str {
        row(&self.table, key).0
    }

    pub(crate) fn number(&self, key: &str) -> f64 {
        self.value(key).parse().unwrap()
    }

    // Sample input: `00 00 80 3f 00 00 00 bf`, two little endian floats that read 1.0 and -0.5.
    fn floats(bytes: &[u8]) -> Vec<f32> {
        let (samples, rest) = bytes.as_chunks::<SAMPLE_BYTES>();
        assert_eq!(rest, b"");
        samples
            .iter()
            .map(|sample| f32::from_le_bytes(*sample))
            .collect()
    }

    /// The samples of the WAV file the command wrote: what the candidate decoded.
    pub(crate) fn candidate(&self) -> Vec<f32> {
        let wav = fs::read(self.run.join("dictation.f32.wav")).unwrap();
        Self::floats(&wav[WAV_HEADER_BYTES..])
    }

    /// A shell command that writes `frames` frames of the candidate's decode of this run, from frame `from` on.
    pub(crate) fn replay(&self, from: usize, frames: usize) -> String {
        let wav = self.run.join("dictation.f32.wav");
        // `tail -c +N` starts at byte N counted from 1.
        let before = from.saturating_mul(FRAME * SAMPLE_BYTES);
        let first = before.saturating_add(WAV_HEADER_BYTES + 1);
        let bytes = frames.saturating_mul(FRAME * SAMPLE_BYTES);
        format!("tail -c +{first} '{}' | head -c {bytes}", wav.display())
    }

    /// Each frame of the candidate's decode laid on the decode `oracle` left in the run: their largest difference and the largest sample of either, in steps of a 16 bit sample.
    pub(crate) fn frames_apart(&self, oracle: &str) -> Vec<(f64, f64)> {
        let theirs = Self::floats(&fs::read(self.run.join(format!("{oracle}.f32"))).unwrap());
        let ours = self.candidate();
        assert_eq!(ours.len(), theirs.len(), "{oracle}");
        let frames = ours.chunks(FRAME).zip(theirs.chunks(FRAME));
        frames
            .map(|(ours, theirs)| {
                let apart = ours
                    .iter()
                    .zip(theirs)
                    .map(|(&a, &b)| (f64::from(a) - f64::from(b)).abs() * LSB_PER_FULL_SCALE);
                let peak = Self::peak_lsb(ours).max(Self::peak_lsb(theirs));
                (apart.fold(0.0, f64::max), peak)
            })
            .collect()
    }

    /// The largest sample of a frame, in steps of a 16 bit sample.
    pub(crate) fn peak_lsb(frame: &[f32]) -> f64 {
        let levels = frame
            .iter()
            .map(|&sample| f64::from(sample).abs() * LSB_PER_FULL_SCALE);
        levels.fold(0.0, f64::max)
    }

    /// The level of a frame in decibels: ten times the logarithm of its mean square.
    pub(crate) fn level_db(frame: &[f32]) -> f64 {
        let squares: f64 = frame.iter().map(|&sample| f64::from(sample).powi(2)).sum();
        10.0 * (squares / count(frame.len())).log10()
    }

    /// How far two columns of one length move together, from -1 to 1.
    pub(crate) fn correlation(xs: &[f64], ys: &[f64]) -> f64 {
        let mean = |values: &[f64]| values.iter().sum::<f64>() / count(values.len());
        let (mean_x, mean_y) = (mean(xs), mean(ys));
        let pairs = || xs.iter().zip(ys).map(|(x, y)| (x - mean_x, y - mean_y));
        let both: f64 = pairs().map(|(x, y)| x * y).sum();
        let of_x: f64 = pairs().map(|(x, _)| x * x).sum();
        let of_y: f64 = pairs().map(|(_, y)| y * y).sum();
        both / (of_x * of_y).sqrt()
    }
}

/// Replays the CLI's deterministic `0x5eed_0017` mutation draw over `frames` and returns `(expected_refusals, first_0x5d_packet)`.
pub(crate) fn expected_mutation_refusals(
    frames: &[Sent],
    mutations: usize,
) -> (usize, Option<Vec<u8>>) {
    const MUTATION_SEED: u64 = 0x5eed_0017;
    const DAMAGES: usize = 3;
    let below = |rng: &mut u64, limit: usize| -> usize {
        let lim = u64::try_from(limit).unwrap();
        usize::try_from(next_random(rng).checked_rem(lim).unwrap()).unwrap()
    };
    let mut rng = MUTATION_SEED;
    let mut refused = 0_usize;
    let mut toc_5d = None;
    for _ in 0..mutations {
        let frame = &frames[below(&mut rng, frames.len())].data;
        let damaged = match below(&mut rng, DAMAGES) {
            0 => {
                let mut out = frame.clone();
                let bit = below(&mut rng, frame.len().saturating_mul(8));
                let byte = bit / 8;
                let in_byte = u32::try_from(bit % 8).unwrap();
                out[byte] ^= 1_u8.rotate_left(in_byte);
                out
            }
            1 => {
                let kept = below(&mut rng, frame.len());
                frame[..kept].to_vec()
            }
            _ => {
                let mut out = frame.clone();
                let [first, ..] = next_random(&mut rng).to_le_bytes();
                if let Some(slot) = out.first_mut() {
                    *slot = first;
                }
                out
            }
        };
        if damaged.first() == Some(&0x5d) && toc_5d.is_none() {
            toc_5d = Some(damaged.clone());
        }
        let invalid = damaged.is_empty()
            || damaged.len() > pods_opus::MAX_PACKET_BYTES
            || damaged.first() != Some(&TOC_CELT_WIDEBAND_20MS_MONO);
        if invalid {
            refused = refused.saturating_add(1);
        }
    }
    (refused, toc_5d)
}
