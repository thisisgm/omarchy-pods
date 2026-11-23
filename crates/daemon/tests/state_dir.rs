//! `librepods state-dir` names the directory the built daemon keeps its state file in, the one an uninstall removes.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    use super::support::{DAEMON_LOG, Session, binary, run_to_end, wait_for};

    /// The session's command for `args`, with `XDG_STATE_HOME` taken away when `unset` asks for the home's fallback.
    fn command(session: &Session, args: &[&str], unset: bool) -> Command {
        let mut command = session.command(Path::new(binary()), args);
        if unset {
            command.env_remove("XDG_STATE_HOME");
        }
        command
    }

    // Sample input: `/tmp/.tmpAb12/state/librepods\n`.
    fn named_dir(session: &Session, unset: bool) -> PathBuf {
        let mut lookup = command(session, &["state-dir"], unset);
        let output = run_to_end(lookup.stdout(Stdio::piped()).stderr(Stdio::piped()));
        assert!(output.status.success(), "{output:?}");
        let mut line = output.stdout;
        assert_eq!(line.pop(), Some(b'\n'), "{line:?}");
        PathBuf::from(OsString::from_vec(line))
    }

    #[test]
    fn the_daemon_writes_its_state_file_in_the_directory_state_dir_names() {
        let mut inside = Vec::new();
        for unset in [false, true] {
            let session = Session::new();
            let dir = named_dir(&session, unset);
            let Ok(relative) = dir.strip_prefix(session.dir.path()) else {
                panic!("{} is outside the session", dir.display());
            };
            inside.push(relative.to_path_buf());
            let mut daemon =
                session.spawn_command(&mut command(&session, &["daemon"], unset), DAEMON_LOG);
            wait_for("the state file", || {
                dir.join("status.json").is_file().then_some(())
            });
            daemon.stop();
        }
        assert_ne!(
            inside[0], inside[1],
            "the home's fallback named the XDG_STATE_HOME directory"
        );
    }
}
