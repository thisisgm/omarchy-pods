//! `cargo xtask samples opus <dir>`: codes the two tones of `pods-samples` again as new files in `<dir>`, from sines made here, with the installed FFmpeg and its libopus; other versions of either may code other bytes.

use std::f64::consts::PI;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::thread;

use pods_mic::opus::MAX_OPUS_FRAME_BYTES;
use pods_proto::dictation::TOC_CELT_WIDEBAND_20MS_MONO;
use rustix::fs::{AtFlags, CWD, Mode, OFlags};

use crate::probe::oggfile;
use crate::{print_line, report};

const CODER: &str = "ffmpeg";
/// The sine of the tone, the sine of the mark, and the peak of both against full scale.
const TONE_HZ: u32 = 1000;
const MARK_HZ: u32 = 2500;
const TONE_DBFS: f64 = -30.0;
/// The decibels of a tenfold amplitude.
const DB_A_DECADE: f64 = 20.0;
/// Where the committed files are, from this crate's directory, and each file with its sine.
const DATA_DIR: &str = "../samples/data";
const FIXTURES: [(&str, u32); 2] = [("opus-tone.bin", TONE_HZ), ("opus-mark.bin", MARK_HZ)];
/// The most a committed fixture is read to: about sixteen times a coded one of 4050 bytes, so an enlarged file is refused, not held.
const FIXTURE_LIMIT_BYTES: u64 = 64 * 1024;
/// The sample rate of the pods' microphone.
const RATE_HZ: u32 = 16_000;
/// How long each sine is: more than the frames skipped and kept.
const SECONDS: u32 = 3;
/// The two packets an Ogg Opus stream opens with, which are no audio.
const HEADERS: usize = 2;
/// The frames left out while the coder settles, then the frames kept: one second.
const SKIPPED: usize = 25;
const KEPT: usize = 50;
/// How the sine is read: raw little endian floats.
const INPUT: [&str; 5] = ["-hide_banner", "-loglevel", "error", "-f", "f32le"];
/// The coder of the stream: CELT only, a frame every 20 ms, 32 kbit/s at a constant rate, which gives frames of 80 bytes.
const CODING: [&str; 10] = [
    "-c:a",
    "libopus",
    "-application",
    "lowdelay",
    "-frame_duration",
    "20",
    "-b:a",
    "32k",
    "-vbr",
    "off",
];
/// The stream comes back as Ogg Opus.
const OUTPUT: [&str; 2] = ["-f", "opus"];

pub(crate) fn run(out: &Path) -> ExitCode {
    match make(out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(wrong) => {
            report(&format!("xtask: {wrong}"));
            ExitCode::FAILURE
        }
    }
}

/// Codes every fixture, then writes each as a new file in `out` and says whether it is the committed one; what can refuse the run is looked at before anything is coded or made.
fn make(out: &Path) -> Result<(), String> {
    match fs::metadata(out) {
        Ok(found) if found.is_dir() => {}
        Ok(_) => return Err(format!("{} is no directory", out.display())),
        Err(error) => return Err(format!("could not look at {}: {error}", out.display())),
    }
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join(DATA_DIR);
    let mut committed = Vec::new();
    for (file, _) in FIXTURES {
        free(&out.join(file))?;
        committed.push(read_committed(&data.join(file))?);
    }
    let mut coded = Vec::new();
    for (_, hz) in FIXTURES {
        let stream = code(hz)?;
        let packets = oggfile::packets(&stream)
            .map_err(|error| format!("{CODER} wrote no Ogg stream for {hz} Hz: {error}"))?;
        coded.push(fixture(&packets).map_err(|wrong| format!("{hz} Hz: {wrong}"))?);
    }
    // Every fixture is whole before the first gets its name, so a write that fails names nothing.
    let mut written = Vec::new();
    for frames in &coded {
        let unnamed = write_unnamed(out, frames).map_err(|error| {
            let dir = out.display();
            format!("could not write in {dir}: {error}; nothing was made")
        })?;
        written.push(unnamed);
    }
    let mut named = Vec::new();
    let made = committed.iter().zip(&coded).zip(&written);
    for ((file, hz), ((committed, frames), unnamed)) in FIXTURES.into_iter().zip(made) {
        let path = out.join(file);
        // Nothing here takes a file away, so a name that fails says which names stay.
        name(unnamed, &path).map_err(|error| {
            let path = path.display();
            format!("could not name {path}: {error}; this run named {named:?} before it")
        })?;
        named.push(file);
        let verdict = if committed == frames {
            "as committed"
        } else {
            "changed"
        };
        let bytes = frames.len();
        print_line(&format!(
            "{file}: {hz} Hz, {KEPT} frames, {bytes} bytes, {verdict}"
        ));
    }
    Ok(())
}

/// Refuses a name that is taken, by a symbolic link too, or that cannot be looked for.
fn free(path: &Path) -> Result<(), String> {
    match path.symlink_metadata() {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(format!(
            "{} is in the way; nothing was made",
            path.display()
        )),
        Err(error) => Err(format!("could not look for {}: {error}", path.display())),
    }
}

/// The bytes of a committed fixture; one that cannot be read is an error and never a difference.
fn read_committed(path: &Path) -> Result<Vec<u8>, String> {
    let unread = |error: io::Error| format!("could not read {}: {error}", path.display());
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(unread)?
        .take(FIXTURE_LIMIT_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(unread)?;
    if !u64::try_from(bytes.len()).is_ok_and(|len| len <= FIXTURE_LIMIT_BYTES) {
        return Err(format!(
            "{} is over {FIXTURE_LIMIT_BYTES} bytes; nothing was made",
            path.display()
        ));
    }
    Ok(bytes)
}

/// Writes `bytes` to a file of mode 0600 in `dir` that has no name yet.
fn write_unnamed(dir: &Path, bytes: &[u8]) -> io::Result<File> {
    let flags = OFlags::WRONLY | OFlags::TMPFILE | OFlags::CLOEXEC;
    let mut file = File::from(rustix::fs::open(dir, flags, Mode::RUSR | Mode::WUSR)?);
    file.write_all(bytes)?;
    Ok(file)
}

/// Gives the unnamed `file` the name `path`; a taken name, a symbolic link included, is refused and never followed.
fn name(file: &File, path: &Path) -> io::Result<()> {
    // corner: Linux only, a file with no name is given one through its entry under /proc/self/fd
    let unnamed = format!("/proc/self/fd/{}", file.as_raw_fd());
    let follow = AtFlags::SYMLINK_FOLLOW;
    rustix::fs::linkat(CWD, unnamed, CWD, path, follow)?;
    Ok(())
}

#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "a sample inside full scale, as the float the coder reads"
)]
fn narrow(sample: f64) -> f32 {
    sample as f32
}

/// `SECONDS` of the sine at `hz` with its peak at `TONE_DBFS`, as little endian floats.
fn sine(hz: u32) -> Vec<u8> {
    let peak = 10_f64.powf(TONE_DBFS / DB_A_DECADE);
    let samples = 0..RATE_HZ.saturating_mul(SECONDS);
    let coded = samples.flat_map(|sample| {
        let turned = 2.0 * PI * f64::from(hz) * f64::from(sample) / f64::from(RATE_HZ);
        narrow(peak * turned.sin()).to_le_bytes()
    });
    coded.collect()
}

/// Codes the sine at `hz`: the samples go in on the coder's standard input and the Ogg stream comes back on its standard output.
fn code(hz: u32) -> Result<Vec<u8>, String> {
    let rate = RATE_HZ.to_string();
    let shape = ["-ar", &rate, "-ac", "1"];
    // The coder's standard error is this command's, so what it cannot code is named there.
    let child = Command::new(CODER)
        .args(INPUT)
        .args(shape)
        .args(["-i", "pipe:0"])
        .args(CODING)
        .args(shape)
        .args(OUTPUT)
        .arg("pipe:1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Err(format!("`{CODER}` is not on the PATH"));
        }
        Err(error) => return Err(format!("could not start {CODER}: {error}")),
    };
    let sine = sine(hz);
    let input = child.stdin.take();
    let output = thread::scope(|scope| {
        // Written from a second thread, since the coder sends its stream while it still reads; a coder that stops reading fails by its status.
        scope.spawn(move || input.map(|mut input| input.write_all(&sine)));
        child.wait_with_output()
    });
    match output {
        Ok(output) if output.status.success() => Ok(output.stdout),
        Ok(output) => Err(format!("{CODER} failed with {}", output.status)),
        Err(error) => Err(format!("could not read from {CODER}: {error}")),
    }
}

/// The kept frames of one coded stream, a length byte in front of each, or what is wrong with the stream.
// Sample input: two header packets, then a packet a frame, each `b8 ..` of at most 94 bytes.
fn fixture(packets: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    let audio = packets.get(HEADERS..).unwrap_or_default();
    let needed = SKIPPED.saturating_add(KEPT);
    let kept = audio
        .get(SKIPPED..needed)
        .ok_or_else(|| format!("{} frames coded, {needed} needed", audio.len()))?;
    let mut file = Vec::new();
    for frame in kept {
        let len = u8::try_from(frame.len()).ok();
        let len = len.filter(|len| usize::from(*len) <= MAX_OPUS_FRAME_BYTES);
        match (frame.first(), len) {
            (Some(&TOC_CELT_WIDEBAND_20MS_MONO), Some(len)) => {
                file.push(len);
                file.extend_from_slice(frame);
            }
            (opens, _) => {
                let bytes = frame.len();
                return Err(format!(
                    "a frame of {bytes} bytes that opens with {opens:02x?} is not CELT only, wideband, 20 ms, mono and at most {MAX_OPUS_FRAME_BYTES} bytes"
                ));
            }
        }
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use pods_samples::opus;

    use super::*;

    /// A coded stream of `frames` frames, each its own number behind the byte `opens`.
    fn stream(frames: u8, opens: u8) -> Vec<Vec<u8>> {
        let headers = [b"OpusHead".to_vec(), b"OpusTags".to_vec()];
        let audio = (0..frames).map(|frame| vec![opens, frame]);
        headers.into_iter().chain(audio).collect()
    }

    #[test]
    fn the_maker_codes_the_sines_the_samples_name() {
        assert_eq!(
            (TONE_HZ, MARK_HZ, TONE_DBFS),
            (opus::TONE_HZ, opus::MARK_HZ, f64::from(opus::TONE_DBFS))
        );
        assert_eq!(opus::tone().count(), KEPT);
        assert_eq!(opus::mark().count(), KEPT);
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join(DATA_DIR);
        let committed = FIXTURES.map(|(file, _)| fs::read(data.join(file)).unwrap());
        let frames: [Vec<&[u8]>; 2] = [opus::tone().collect(), opus::mark().collect()];
        let files = frames.map(|frames| {
            let prefixed = frames.iter().map(|frame| {
                let len = u8::try_from(frame.len()).unwrap();
                [&[len], *frame].concat()
            });
            prefixed.collect::<Vec<_>>().concat()
        });
        assert_eq!(committed, files);
    }

    #[test]
    fn a_sine_is_three_seconds_of_floats_that_peak_30_db_under_full_scale() {
        let peak = narrow(10_f64.powf(-1.5));
        for hz in [TONE_HZ, MARK_HZ] {
            let samples: Vec<f32> = sine(hz)
                .as_chunks::<4>()
                .0
                .iter()
                .map(|sample| f32::from_le_bytes(*sample))
                .collect();
            assert_eq!(samples.len(), 48_000);
            assert_eq!(samples[0], 0.0);
            let highest = samples.iter().copied().fold(0.0, f32::max);
            assert!((highest - peak).abs() <= peak * 1e-3, "{hz} Hz: {highest}");
            // A second later the sine stands where it began, to the last bit of the angle.
            let late = samples[16_000].abs();
            assert!(late < 1e-9, "{hz} Hz: {late}");
        }
    }

    #[test]
    fn a_fixture_has_no_name_until_it_is_whole_and_a_taken_name_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new");
        let whole = write_unnamed(dir.path(), &[1, 2]).unwrap();
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
        name(&whole, &path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), [1, 2]);
        let other = write_unnamed(dir.path(), &[3]).unwrap();
        let again = name(&other, &path).unwrap_err();
        assert_eq!(again.kind(), ErrorKind::AlreadyExists);
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        let followed = name(&other, &link).unwrap_err();
        assert_eq!(followed.kind(), ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&path).unwrap(), [1, 2]);
        // A refused name leaves no other name behind.
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
        let nowhere = write_unnamed(&dir.path().join("none"), &[3]).unwrap_err();
        assert_eq!(nowhere.kind(), ErrorKind::NotFound);
    }

    #[test]
    fn a_taken_name_and_a_committed_file_that_cannot_be_read_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opus-tone.bin");
        assert_eq!(free(&path), Ok(()));
        let unread = read_committed(&path).unwrap_err();
        let said = format!("could not read {}: ", path.display());
        assert!(unread.starts_with(&said), "{unread}");
        name(&write_unnamed(dir.path(), &[1]).unwrap(), &path).unwrap();
        assert_eq!(read_committed(&path), Ok(vec![1]));
        let large = dir.path().join("large.bin");
        let at_limit = usize::try_from(FIXTURE_LIMIT_BYTES).unwrap();
        fs::write(&large, vec![0; at_limit]).unwrap();
        assert_eq!(
            read_committed(&large).map(|bytes| bytes.len()),
            Ok(at_limit)
        );
        fs::write(&large, vec![0; at_limit + 1]).unwrap();
        let over = format!(
            "{} is over {FIXTURE_LIMIT_BYTES} bytes; nothing was made",
            large.display()
        );
        assert_eq!(read_committed(&large).map(|bytes| bytes.len()), Err(over));
        // corner: Linux only; an endless file is refused at all only because the read stops at the limit.
        let endless = Path::new("/dev/zero");
        let refused = format!(
            "{} is over {FIXTURE_LIMIT_BYTES} bytes; nothing was made",
            endless.display()
        );
        assert_eq!(
            read_committed(endless).map(|bytes| bytes.len()),
            Err(refused)
        );
        let taken = format!("{} is in the way; nothing was made", path.display());
        assert_eq!(free(&path), Err(taken));
        // A link to nothing takes its name all the same.
        let link = dir.path().join("link");
        std::os::unix::fs::symlink("nowhere", &link).unwrap();
        assert!(free(&link).is_err());
    }

    #[test]
    fn the_fixture_is_the_second_behind_the_frames_skipped() {
        let made = fixture(&stream(80, TOC_CELT_WIDEBAND_20MS_MONO)).unwrap();
        let expected: Vec<u8> = (25..75)
            .flat_map(|frame| [2, TOC_CELT_WIDEBAND_20MS_MONO, frame])
            .collect();
        assert_eq!(made, expected);
        let exact = fixture(&stream(75, TOC_CELT_WIDEBAND_20MS_MONO));
        assert_eq!(exact.unwrap(), expected);
    }

    #[test]
    fn a_stream_that_is_not_the_one_the_pods_send_makes_no_fixture() {
        let short = fixture(&stream(74, TOC_CELT_WIDEBAND_20MS_MONO));
        assert_eq!(short.unwrap_err(), "74 frames coded, 75 needed");
        assert_eq!(fixture(&[]).unwrap_err(), "0 frames coded, 75 needed");
        let other = fixture(&stream(80, 0xb0)).unwrap_err();
        assert!(
            other.starts_with("a frame of 2 bytes that opens with Some(b0)"),
            "{other}"
        );
        for (len, fits) in [(94, true), (95, false), (300, false)] {
            let mut long = stream(80, TOC_CELT_WIDEBAND_20MS_MONO);
            long[HEADERS + SKIPPED].resize(len, 0);
            assert_eq!(fixture(&long).is_ok(), fits, "{len} bytes");
        }
        let mut empty = stream(80, TOC_CELT_WIDEBAND_20MS_MONO);
        empty[HEADERS + SKIPPED + KEPT - 1].clear();
        let none = fixture(&empty).unwrap_err();
        assert!(
            none.starts_with("a frame of 0 bytes that opens with None"),
            "{none}"
        );
    }
}
