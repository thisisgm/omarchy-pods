//! The file under test: what it says it is, and that a session starts the daemon `LIBREPODS_E2E_BINARY` names.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "these tests start one daemon and read its version, so the rig and the fakes the other suites share go unused"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::env;
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitStatus, Stdio};

    use super::support::{self, BINARY_VARIABLE, Session, run_to_end};

    /// The test a respawned copy of this test binary runs, by its full name.
    const UNDER_TEST: &str = "tests::the_daemon_a_session_starts_is_the_file_under_test";

    /// Runs `UNDER_TEST` alone in a new process of this test binary in `dir`, with `BINARY_VARIABLE` naming `file`.
    fn respawn_with(dir: &Path, file: &Path) -> (ExitStatus, String) {
        let mut command = Command::new(env::current_exe().unwrap());
        command
            .args(["--exact", UNDER_TEST])
            .current_dir(dir)
            .env(BINARY_VARIABLE, file)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = run_to_end(&mut command);
        // The harness prints a panic to stdout when it captures and to stderr when told not to.
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (output.status, said)
    }

    #[test]
    fn version_flag_prints_name_and_version() {
        for flag in ["--version", "-V"] {
            let out = Command::new(support::binary()).arg(flag).output().unwrap();
            assert!(out.status.success(), "{flag}");
            assert!(out.stderr.is_empty(), "{flag}");
            assert_eq!(
                String::from_utf8(out.stdout).unwrap(),
                format!("librepods {}\n", env!("CARGO_PKG_VERSION"))
            );
        }
    }

    #[test]
    fn help_flags_and_subcommand_print_usage_to_stdout() {
        for (args, usage) in [
            (&["-h"][..], "librepods <COMMAND>"),
            (&["--help"][..], "librepods <COMMAND>"),
            (&["help"][..], "librepods <COMMAND>"),
            (&["help", "set"][..], "librepods set <KEY> <VALUE>"),
            (
                &["set", "noise", "--help"][..],
                "librepods set <KEY> <VALUE>",
            ),
            (&["do", "-h"][..], "librepods do <ACTION>"),
            (
                &["--headless", "--help"][..],
                "librepods {daemon|--headless}",
            ),
        ] {
            let out = Command::new(support::binary()).args(args).output().unwrap();
            assert!(out.status.success(), "{args:?}");
            assert!(out.stderr.is_empty(), "{args:?}");
            let text = String::from_utf8(out.stdout).unwrap();
            assert!(text.contains(&format!("\nUsage: {usage}\n")), "{text}");
        }
    }

    #[test]
    fn parser_failures_print_usage_to_stderr_and_exit_two() {
        for (args, usage, reason) in [
            (&[][..], "librepods <COMMAND>", "Commands:"),
            (
                &["--bogus"][..],
                "librepods <COMMAND>",
                "unexpected argument '--bogus'",
            ),
            (
                &["bogus"][..],
                "librepods <COMMAND>",
                "unrecognized subcommand 'bogus'",
            ),
            (
                &["set", "noise"][..],
                "librepods set <KEY> <VALUE>",
                "\n  <VALUE>\n",
            ),
            (
                &["do", "connect", "extra"][..],
                "librepods do <ACTION>",
                "unexpected argument 'extra'",
            ),
        ] {
            let out = Command::new(support::binary()).args(args).output().unwrap();
            assert_eq!(out.status.code(), Some(2), "{args:?}");
            assert!(out.stdout.is_empty(), "{args:?}");
            let text = String::from_utf8(out.stderr).unwrap();
            assert!(text.contains(&format!("\nUsage: {usage}\n")), "{text}");
            assert!(text.contains(reason), "{text}");
        }
    }

    #[test]
    fn legacy_client_commands_ignore_trailing_non_utf8_arguments() {
        let session = Session::new();
        let _daemon = session.start();
        let dir = tempfile::tempdir().unwrap();
        let ctl = dir.path().join("librepods-ctl");
        symlink(support::binary(), &ctl).unwrap();
        let invalid = OsString::from_vec(vec![0xff]);
        let ctl_args = [OsString::from("status"), invalid.clone()];
        let sub_args = [OsString::from("ctl"), OsString::from("status"), invalid];

        for (program, args) in [
            (ctl.as_os_str(), ctl_args.as_slice()),
            (OsStr::new(support::binary()), sub_args.as_slice()),
        ] {
            let out = Command::new(program)
                .args(args)
                .env("XDG_RUNTIME_DIR", session.runtime_dir())
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{program:?} {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(out.stderr.is_empty(), "{program:?} {args:?}");
            let reply = String::from_utf8(out.stdout).unwrap();
            assert!(reply.trim().starts_with('{'), "{reply}");
        }
    }

    #[test]
    fn the_daemon_a_session_starts_is_the_file_under_test() {
        let wanted = env::var_os(BINARY_VARIABLE).map_or_else(
            || PathBuf::from(env!("CARGO_BIN_EXE_librepods")),
            PathBuf::from,
        );
        let session = Session::new();
        let daemon = session.start();
        let running = fs::read_link(format!("/proc/{}/exe", daemon.child.id())).unwrap();
        assert_eq!(running, fs::canonicalize(wanted).unwrap());
    }

    #[test]
    fn a_session_starts_the_file_the_variable_names() {
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join("librepods-under-test");
        fs::copy(support::binary(), &copy).unwrap();
        let (status, said) = respawn_with(dir.path(), &copy);
        assert!(status.success(), "{said}");
        assert!(said.contains("test result: ok. 1 passed"), "{said}");
    }

    #[test]
    fn a_bare_name_in_the_variable_is_the_file_in_the_directory_the_suite_runs_in() {
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join("librepods-under-test");
        fs::copy(support::binary(), &copy).unwrap();
        let (status, said) = respawn_with(dir.path(), Path::new("librepods-under-test"));
        assert!(status.success(), "{said}");
        assert!(said.contains("test result: ok. 1 passed"), "{said}");
    }

    #[test]
    fn a_variable_that_names_no_file_fails_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let nothing = dir.path().join("nothing");
        let (status, said) = respawn_with(dir.path(), &nothing);
        assert!(!status.success(), "{said}");
        let sentence = format!(
            "{BINARY_VARIABLE} names {}, which is no file: ",
            nothing.display()
        );
        assert!(said.contains(&sentence), "{said}");
    }

    #[test]
    fn a_variable_that_names_a_directory_fails_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let (status, said) = respawn_with(dir.path(), dir.path());
        assert!(!status.success(), "{said}");
        let sentence = format!(
            "{BINARY_VARIABLE} names {}, which is no file\n",
            dir.path().display()
        );
        assert!(said.contains(&sentence), "{said}");
    }
}
