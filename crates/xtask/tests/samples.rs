//! `cargo xtask samples opus <dir>` through the built binary: with no coder, with a stand-in that fails, sends no stream, or sends the committed tone, and with a file that cannot be written whole.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use std::fmt::Write;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::{Command, Output};

    use pods_mic::opus::MAX_OPUS_FRAME_BYTES;
    use pods_proto::dictation::TOC_CELT_WIDEBAND_20MS_MONO;
    use pods_samples::opus::tone;
    use tempfile::TempDir;

    /// The two files the maker names, in the order a directory is listed here.
    const MADE: [&str; 2] = ["opus-mark.bin", "opus-tone.bin"];
    /// The bytes of an Ogg page between its pattern and its count of lacing values, which the maker never reads.
    const PAGE_UNREAD: usize = 22;
    /// The packets a stream opens with that are no audio, and the frames the maker leaves out before the second it keeps.
    const HEADERS: usize = 2;
    const SKIPPED: usize = 25;
    /// The most a file may hold in one test, in the shell's blocks of 512 bytes: the committed tone fits and as many frames of the most bytes do not.
    const LIMIT_BLOCKS: u32 = 8;

    /// A directory for the coder's stand-in, which is the whole PATH, and one for the fixtures.
    fn dirs() -> (TempDir, TempDir) {
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap())
    }

    /// Puts an `ffmpeg` in `dir` that runs `script`, which has only the shell's own commands: `dir` is the whole PATH.
    fn stand_in(dir: &Path, script: &str) {
        let path = dir.join("ffmpeg");
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// One Ogg page that holds `packet` alone, which is shorter than a full lacing value.
    // Sample output: `4f 67 67 53`, 22 zero bytes, `01 03`, `b8 10 11`: one packet of three bytes.
    fn page(packet: &[u8]) -> Vec<u8> {
        let mut page = b"OggS".to_vec();
        page.extend([0; PAGE_UNREAD]);
        page.extend([1, u8::try_from(packet.len()).unwrap()]);
        page.extend(packet);
        page
    }

    /// The Ogg stream of `frames` behind the headers and the frames skipped, as the octal escapes a `printf` sends.
    fn stream_of<'a>(frames: impl Iterator<Item = &'a [u8]>) -> String {
        let opening = vec![&[TOC_CELT_WIDEBAND_20MS_MONO][..]; HEADERS + SKIPPED];
        let stream: Vec<u8> = opening.into_iter().chain(frames).flat_map(page).collect();
        let mut octal = String::new();
        for byte in stream {
            write!(octal, "\\{byte:03o}").unwrap();
        }
        octal
    }

    /// The script of a coder that leaves a mark of its run and sends the committed tone behind the headers and the frames skipped, whatever sine it is given.
    fn coder_of_the_tone() -> String {
        format!(": > \"$0.ran\"\nprintf '{}'", stream_of(tone()))
    }

    /// The script of a coder that sends the committed tone on its first run and as many frames of the most bytes on every later one.
    fn coder_of_a_longer_second_stream() -> String {
        let full = [
            &[TOC_CELT_WIDEBAND_20MS_MONO][..],
            &[0; MAX_OPUS_FRAME_BYTES - 1],
        ]
        .concat();
        let longer = stream_of(std::iter::repeat_n(&full[..], tone().count()));
        let first = coder_of_the_tone();
        format!("if [ -e \"$0.ran\" ]; then printf '{longer}'; exit; fi\n{first}")
    }

    /// The names `dir` holds, sorted.
    fn names(dir: &Path) -> Vec<String> {
        let held = fs::read_dir(dir).unwrap();
        let mut names: Vec<String> = held
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    fn samples(path: &Path, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_xtask"))
            .arg("samples")
            .args(arguments)
            .env("PATH", path)
            .output()
            .unwrap()
    }

    /// The maker run for `out` where no file may pass `LIMIT_BLOCKS`; the signal of the limit is ignored, so the write past it fails.
    fn samples_under_the_limit(path: &Path, out: &Path) -> Output {
        let limited = format!("ulimit -f {LIMIT_BLOCKS}; trap '' XFSZ; exec \"$@\"");
        let maker = [env!("CARGO_BIN_EXE_xtask"), "samples", "opus"];
        Command::new("/bin/sh")
            .args(["-c", &limited, "sh"])
            .args(maker)
            .arg(out)
            .env("PATH", path)
            .output()
            .unwrap()
    }

    /// The command with `arguments` fails with `said` on its standard error and leaves `out` empty.
    fn assert_makes_nothing(path: &Path, arguments: &[&str], out: &Path, said: &str) {
        let output = samples(path, arguments);
        assert_eq!(output.status.code(), Some(1));
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.starts_with(said), "{error}");
        assert_eq!(output.stdout, b"");
        assert_eq!(fs::read_dir(out).unwrap().count(), 0);
    }

    #[test]
    fn anything_but_the_one_kind_and_a_directory_is_refused_before_the_coder_runs() {
        let (path, out) = dirs();
        stand_in(path.path(), ": > \"$0.ran\"");
        let dir = out.path().to_str().unwrap();
        for arguments in [&[][..], &["opus"], &["aac", dir], &["opus", dir, "--check"]] {
            assert_makes_nothing(path.path(), arguments, out.path(), "usage: cargo xtask ");
        }
        assert!(!path.path().join("ffmpeg.ran").exists(), "the coder ran");
    }

    #[test]
    fn without_the_coder_nothing_is_made() {
        let (path, out) = dirs();
        let arguments = ["opus", out.path().to_str().unwrap()];
        let said = "xtask: `ffmpeg` is not on the PATH\n";
        assert_makes_nothing(path.path(), &arguments, out.path(), said);
    }

    #[test]
    fn a_coder_that_fails_makes_nothing() {
        let (path, out) = dirs();
        let arguments = ["opus", out.path().to_str().unwrap()];
        // It never reads the sine, so the write to it fails, and only its status is reported.
        stand_in(path.path(), "exit 3");
        let said = "xtask: ffmpeg failed with exit status: 3\n";
        assert_makes_nothing(path.path(), &arguments, out.path(), said);
    }

    #[test]
    fn a_coder_that_sends_no_stream_makes_nothing() {
        let (path, out) = dirs();
        let arguments = ["opus", out.path().to_str().unwrap()];
        stand_in(path.path(), "exit 0");
        let said = "xtask: 1000 Hz: 0 frames coded, 75 needed\n";
        assert_makes_nothing(path.path(), &arguments, out.path(), said);
        stand_in(path.path(), "printf 'not a stream of pages, only bytes'");
        let said =
            "xtask: ffmpeg wrote no Ogg stream for 1000 Hz: a page does not open with OggS\n";
        assert_makes_nothing(path.path(), &arguments, out.path(), said);
    }

    #[test]
    fn each_fixture_is_made_as_a_new_file_and_told_from_the_committed_one() {
        let (path, out) = dirs();
        stand_in(path.path(), &coder_of_the_tone());
        let output = samples(path.path(), &["opus", out.path().to_str().unwrap()]);
        let error = String::from_utf8_lossy(&output.stderr);
        assert_eq!((output.status.code(), &*error), (Some(0), ""));
        // The mark a refusal is held to elsewhere: a coder that ran left it.
        assert!(
            path.path().join("ffmpeg.ran").exists(),
            "no mark of the coder"
        );
        let committed: Vec<u8> = tone()
            .flat_map(|frame| [&[u8::try_from(frame.len()).unwrap()], frame].concat())
            .collect();
        // The stand-in codes the tone for both sines, so the tone is the committed file and the mark is not.
        let bytes = committed.len();
        let said = format!(
            "opus-tone.bin: 1000 Hz, 50 frames, {bytes} bytes, as committed\nopus-mark.bin: 2500 Hz, 50 frames, {bytes} bytes, changed\n"
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), said);
        assert_eq!(names(out.path()), MADE);
        for file in MADE {
            let fixture = out.path().join(file);
            assert_eq!(fs::read(&fixture).unwrap(), committed, "{file}");
            let mode = fs::metadata(&fixture).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{file}");
        }
    }

    #[test]
    fn a_write_that_fails_on_the_second_fixture_names_neither() {
        // The limit lets the committed tone through twice, so what fails below is the longer stream alone.
        let (path, out) = dirs();
        stand_in(path.path(), &coder_of_the_tone());
        let output = samples_under_the_limit(path.path(), out.path());
        let error = String::from_utf8_lossy(&output.stderr);
        assert_eq!((output.status.code(), &*error), (Some(0), ""));
        assert_eq!(names(out.path()), MADE);

        let (path, out) = dirs();
        stand_in(path.path(), &coder_of_a_longer_second_stream());
        let output = samples_under_the_limit(path.path(), out.path());
        let said = format!(
            "xtask: could not write in {}: File too large (os error 27); nothing was made\n",
            out.path().display()
        );
        assert_eq!(String::from_utf8_lossy(&output.stderr), said);
        assert_eq!((output.status.code(), &*output.stdout), (Some(1), &b""[..]));
        assert_eq!(fs::read_dir(out.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_name_in_the_way_or_no_directory_is_refused_before_the_coder_runs() {
        let (path, out) = dirs();
        stand_in(path.path(), &coder_of_the_tone());
        // The second file the maker would name is there already, so the first is not made either.
        let [in_the_way, _] = MADE;
        let old = out.path().join(in_the_way);
        fs::write(&old, b"old").unwrap();
        let output = samples(path.path(), &["opus", out.path().to_str().unwrap()]);
        let said = format!("xtask: {} is in the way; nothing was made\n", old.display());
        assert_eq!(String::from_utf8_lossy(&output.stderr), said);
        assert_eq!((output.status.code(), &*output.stdout), (Some(1), &b""[..]));
        assert_eq!(names(out.path()), [in_the_way]);
        assert_eq!(fs::read(&old).unwrap(), b"old");

        let output = samples(path.path(), &["opus", old.to_str().unwrap()]);
        let said = format!("xtask: {} is no directory\n", old.display());
        assert_eq!(String::from_utf8_lossy(&output.stderr), said);
        assert_eq!(output.status.code(), Some(1));
        // A directory that cannot be looked at is said to be that, with the reason.
        let none = out.path().join("none");
        let output = samples(path.path(), &["opus", none.to_str().unwrap()]);
        let said = format!(
            "xtask: could not look at {}: No such file or directory (os error 2)\n",
            none.display()
        );
        assert_eq!(String::from_utf8_lossy(&output.stderr), said);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(names(out.path()), [in_the_way]);
        assert!(!path.path().join("ffmpeg.ran").exists(), "the coder ran");
    }
}
