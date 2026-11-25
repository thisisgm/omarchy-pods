//! `cargo xtask release-check` through the built binary, against small programs built with `rustc` in the test's own directory.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};
    use std::time::{Duration, Instant};

    /// The version the program prints, as the daemon prints its own.
    const VERSION: &str = "9.8.7";

    /// A two line program that answers `--version` as the daemon does.
    const PROGRAM: &str = "fn main() {\n    println!(\"librepods 9.8.7\");\n}\n";

    /// A program that refuses `--version` with a reason on its standard error.
    const REFUSING: &str =
        "fn main() {\n    eprintln!(\"no such flag\");\n    std::process::exit(3);\n}\n";

    /// A program that never exits.
    const HANGING: &str = "fn main() {\n    loop {\n        std::thread::park();\n    }\n}\n";

    /// A program that ignores `SIGTERM`, which a daemon blocked in its own shutdown does in effect; 15 is `SIGTERM` and 1 is `SIG_IGN`.
    const IGNORING_TERM: &str = "unsafe extern \"C\" {\n    fn signal(number: i32, handler: usize) -> usize;\n}\nfn main() {\n    unsafe { signal(15, 1) };\n    loop {\n        std::thread::park();\n    }\n}\n";

    /// Longer than the check's own 10s deadline and 2s grace; `timeout` exits 124 when it passes.
    const TEST_DEADLINE: &str = "60s";

    const FAST_CHECK_LIMIT: Duration = Duration::from_secs(5);

    /// Checks args like `--kill-after 2s 10s ./librepods --version`, then shortens the real wait.
    const SHORT_TIMEOUT: &str = r#"#!/bin/sh
if [ "$1" != --kill-after ] || [ "$2" != 2s ] || [ "$3" != 10s ]; then
    echo "unexpected release timeout arguments: $*" >&2
    exit 1
fi
shift 3
grace=0.1s
deadline=0.3s
exec /usr/bin/timeout --kill-after "$grace" "$deadline" "$@"
"#;

    const INPUT_DEADLINE: &str = "30s";

    /// Enough for the release check and too little to buffer the large input fixtures, in KiB.
    const INPUT_MEMORY_LIMIT: &str = "65536";

    /// A sparse file larger than the input memory limit.
    const SPARSE_BYTES: u64 = 1 << 30;

    /// What the check says when the file's own crate carries no line table.
    const NO_LINE_TABLES: &str = "FAIL line tables: the librepods crate has no line table, so a backtrace has no file and line";

    /// Links `libresolv.so.2`, which `libc6-dev` ships and the allow list leaves out; without the first flag the linker drops an unused library.
    const OUTSIDE_LIBRARY: [&str; 4] = [
        "-C",
        "link-arg=-Wl,--no-as-needed",
        "-C",
        "link-arg=-lresolv",
    ];

    /// Builds `program` into `dir` as the crate `librepods` with line tables, as the release profile builds the daemon; a later `-C debuginfo` in `flags` wins.
    fn build(dir: &Path, program: &str, flags: &[&str]) -> PathBuf {
        let source = dir.join("main.rs");
        fs::write(&source, program).unwrap();
        let file = dir.join("librepods");
        let status = Command::new("rustc")
            .args(["--edition", "2024", "--crate-name", "librepods"])
            .args(["-C", "debuginfo=line-tables-only"])
            .args(flags)
            .arg("-o")
            .arg(&file)
            .arg(&source)
            .status()
            .unwrap();
        assert!(status.success(), "rustc failed with {status}");
        file
    }

    /// Runs the check under its own deadline, so a check that hangs fails its test instead of hanging it.
    fn release_check(file: &Path, version: &str) -> Output {
        release_check_command(file, version).output().unwrap()
    }

    fn release_check_command(file: &Path, version: &str) -> Command {
        let mut command = Command::new("/usr/bin/timeout");
        command
            .args([TEST_DEADLINE, env!("CARGO_BIN_EXE_xtask"), "release-check"])
            .arg(file)
            .arg(version);
        command
    }

    fn release_check_with_short_timeout(file: &Path) -> Output {
        let timeout = file.with_file_name("timeout");
        fs::write(&timeout, SHORT_TIMEOUT).unwrap();
        fs::set_permissions(&timeout, fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::join_paths([file.parent().unwrap(), Path::new("/usr/bin")]).unwrap();
        release_check_command(file, VERSION)
            .env("PATH", path)
            .output()
            .unwrap()
    }

    fn check_input_under_limits(file: &Path) -> Output {
        Command::new("timeout")
            .args([INPUT_DEADLINE, "sh", "-c"])
            .arg("ulimit -c 0 && ulimit -v \"$1\" && shift && exec \"$@\"")
            .arg("sh")
            .arg(INPUT_MEMORY_LIMIT)
            .args([env!("CARGO_BIN_EXE_xtask"), "release-check"])
            .arg(file)
            .arg(VERSION)
            .output()
            .unwrap()
    }

    fn assert_input_refused(output: &Output, file: &Path, reason: &str) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{stderr}");
        assert_eq!(stderr, format!("xtask: {} {reason}\n", file.display()));
        assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    }

    fn lines(output: &Output) -> Vec<String> {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The four check lines of a run, the size line left out.
    fn checks(output: &Output) -> Vec<String> {
        let mut lines = lines(output);
        let size = lines.pop().unwrap();
        assert!(size.starts_with("size: "), "{size}");
        assert_eq!(lines.len(), 4, "{lines:?}");
        lines
    }

    #[test]
    fn a_binary_that_passes_all_four_exits_zero() {
        let dir = tempfile::tempdir().unwrap();
        let file = build(dir.path(), PROGRAM, &[]);
        let output = release_check(&file, VERSION);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{:?}",
            String::from_utf8_lossy(&output.stderr)
        );
        let lines = lines(&output);
        let bytes = fs::metadata(&file).unwrap().len();
        assert_eq!(lines.len(), 5, "{lines:?}");
        assert_eq!(lines[0], "ok version: librepods 9.8.7");
        assert_eq!(
            lines[1],
            "ok libraries: libgcc_s.so.1 libc.so.6 ld-linux-x86-64.so.2"
        );
        assert!(
            lines[2].starts_with("ok glibc: GLIBC_2.")
                && lines[2].ends_with(", at most GLIBC_2.39"),
            "{}",
            lines[2]
        );
        let source = dir.path().join("main.rs");
        assert_eq!(lines[3], format!("ok line tables: {}", source.display()));
        assert_eq!(lines[4], format!("size: {bytes} bytes"));
    }

    #[test]
    fn a_bare_file_name_runs_the_file_in_this_directory_and_not_one_on_the_path() {
        let dir = tempfile::tempdir().unwrap();
        build(dir.path(), PROGRAM, &[]);
        let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(["release-check", "librepods", VERSION])
            .current_dir(dir.path())
            .env("PATH", "/usr/bin")
            .output()
            .unwrap();
        assert_eq!(checks(&output)[0], "ok version: librepods 9.8.7");
    }

    #[test]
    fn a_wrong_version_fails_and_names_both() {
        let dir = tempfile::tempdir().unwrap();
        let file = build(dir.path(), PROGRAM, &[]);
        for wrong in ["9.8.8", "8.7"] {
            let output = release_check(&file, wrong);
            assert_eq!(output.status.code(), Some(1));
            let checks = checks(&output);
            assert_eq!(
                checks[0],
                format!("FAIL version: `librepods 9.8.7` does not end with {wrong}")
            );
            assert!(
                checks[1..].iter().all(|line| line.starts_with("ok ")),
                "{checks:?}"
            );
        }
    }

    #[test]
    fn a_library_outside_the_list_fails_and_names_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = build(dir.path(), PROGRAM, &OUTSIDE_LIBRARY);
        let output = release_check(&file, VERSION);
        assert_eq!(output.status.code(), Some(1));
        let checks = checks(&output);
        assert_eq!(checks[1], "FAIL libraries: libresolv.so.2 is not allowed");
        let others = [&checks[0], &checks[2], &checks[3]];
        assert!(
            others.iter().all(|line| line.starts_with("ok ")),
            "{checks:?}"
        );
    }

    #[test]
    fn a_stripped_binary_fails() {
        let dir = tempfile::tempdir().unwrap();
        let file = build(dir.path(), PROGRAM, &["-C", "strip=debuginfo"]);
        let output = release_check(&file, VERSION);
        assert_eq!(output.status.code(), Some(1));
        let checks = checks(&output);
        assert_eq!(checks[3], NO_LINE_TABLES);
        assert!(
            checks[..3].iter().all(|line| line.starts_with("ok ")),
            "{checks:?}"
        );
    }

    #[test]
    fn a_binary_whose_own_code_has_no_line_tables_fails() {
        let dir = tempfile::tempdir().unwrap();
        let file = build(
            dir.path(),
            PROGRAM,
            &["-C", "debuginfo=0", "-C", "strip=none"],
        );
        let output = release_check(&file, VERSION);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(checks(&output)[3], NO_LINE_TABLES);
    }

    #[test]
    fn a_version_that_exits_non_zero_fails_with_its_reason() {
        let dir = tempfile::tempdir().unwrap();
        let file = build(dir.path(), REFUSING, &[]);
        let output = release_check(&file, VERSION);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            checks(&output)[0],
            format!(
                "FAIL version: `{} --version` exited with exit status: 3: no such flag",
                file.display()
            )
        );
    }

    #[test]
    fn a_version_that_never_exits_fails_at_the_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let file = build(dir.path(), HANGING, &[]);
        let started = Instant::now();
        let output = release_check_with_short_timeout(&file);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            checks(&output)[0],
            format!(
                "FAIL version: `{} --version` did not exit within 10s",
                file.display()
            )
        );
        let elapsed = started.elapsed();
        assert!(elapsed < FAST_CHECK_LIMIT, "the check took {elapsed:?}");
    }

    #[test]
    fn a_version_that_ignores_the_stop_signal_fails_at_the_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let file = build(dir.path(), IGNORING_TERM, &[]);
        let started = Instant::now();
        let output = release_check_with_short_timeout(&file);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            checks(&output)[0],
            format!(
                "FAIL version: `{} --version` ended on SIGKILL, which timeout sends 2s after a missed 10s deadline",
                file.display()
            )
        );
        let elapsed = started.elapsed();
        assert!(elapsed < FAST_CHECK_LIMIT, "the check took {elapsed:?}");
    }

    #[test]
    fn a_binary_without_its_line_section_fails() {
        let dir = tempfile::tempdir().unwrap();
        let file = build(dir.path(), PROGRAM, &[]);
        let removed = Command::new("objcopy")
            .args(["--remove-section", ".debug_line"])
            .arg(&file)
            .status()
            .unwrap();
        assert!(
            removed.success(),
            "objcopy --remove-section failed with {removed}"
        );
        let output = release_check(&file, VERSION);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(checks(&output)[3], NO_LINE_TABLES);
    }

    #[test]
    fn line_tables_moved_to_a_linked_debug_file_fail() {
        let dir = tempfile::tempdir().unwrap();
        let file = build(dir.path(), PROGRAM, &[]);
        let debug = dir.path().join("librepods.debug");
        let kept = Command::new("objcopy")
            .arg("--only-keep-debug")
            .arg(&file)
            .arg(&debug)
            .status()
            .unwrap();
        assert!(
            kept.success(),
            "objcopy --only-keep-debug failed with {kept}"
        );
        let linked = Command::new("objcopy")
            .arg("--strip-debug")
            .arg(format!("--add-gnu-debuglink={}", debug.display()))
            .arg(&file)
            .status()
            .unwrap();
        assert!(
            linked.success(),
            "objcopy --add-gnu-debuglink failed with {linked}"
        );
        let output = release_check(&file, VERSION);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(checks(&output)[3], NO_LINE_TABLES);
    }

    #[test]
    fn a_missing_readelf_is_named_once() {
        let dir = tempfile::tempdir().unwrap();
        let file = build(dir.path(), PROGRAM, &[]);
        let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .arg("release-check")
            .arg(&file)
            .arg(VERSION)
            .env("PATH", dir.path())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "xtask: `readelf` is not on the PATH; it comes with binutils\n"
        );
    }

    #[test]
    fn a_file_that_is_not_elf_fails_with_one_sentence() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("librepods");
        fs::write(&file, "#!/bin/sh\necho librepods 9.8.7\n").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        let output = release_check(&file, VERSION);
        assert_input_refused(&output, &file, "is not an ELF file");

        fs::write(&file, b"\x7fEL").unwrap();
        let output = release_check(&file, VERSION);
        assert_input_refused(&output, &file, "is not an ELF file");
    }

    #[test]
    fn a_character_device_is_refused_before_readelf() {
        let file = Path::new("/dev/zero");
        let output = check_input_under_limits(file);
        assert_input_refused(&output, file, "is not a regular file");
    }

    #[test]
    fn a_directory_is_refused_before_readelf() {
        let dir = tempfile::tempdir().unwrap();
        let output = release_check(dir.path(), VERSION);
        assert_input_refused(&output, dir.path(), "is not a regular file");
    }

    #[test]
    fn a_fifo_is_refused_without_a_writer() {
        use rustix::fs::{CWD, FileType, Mode, mknodat};

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("librepods");
        mknodat(CWD, &file, FileType::Fifo, Mode::RUSR | Mode::WUSR, 0).unwrap();
        let output = check_input_under_limits(&file);
        assert_input_refused(&output, &file, "is not a regular file");
    }

    #[test]
    fn a_large_sparse_file_is_checked_under_a_small_memory_limit() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("librepods");
        fs::File::create(&file)
            .unwrap()
            .set_len(SPARSE_BYTES)
            .unwrap();
        let output = check_input_under_limits(&file);
        assert_input_refused(&output, &file, "is not an ELF file");
    }

    #[test]
    fn a_missing_file_fails_with_one_sentence() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("librepods");
        let output = release_check(&file, VERSION);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!(
                "xtask: cannot read {}: No such file or directory (os error 2)\n",
                file.display()
            )
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    }

    #[test]
    fn a_missing_argument_prints_the_usage() {
        let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(["release-check", "librepods"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let said = String::from_utf8_lossy(&output.stderr);
        assert!(
            said.contains("cargo xtask release-check <file> <version>"),
            "{said}"
        );
    }
}
