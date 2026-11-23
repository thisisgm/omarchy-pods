//! Support tests: the rig against the real `pipewire` program of this machine, its only proof; no user path yet.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;
    use std::ffi::OsStr;
    use std::fs;
    use std::io::Write;
    use std::panic::{self, AssertUnwindSafe};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use pods_pwrig::tone::{CHECKED, SETTLE, assert_tone, frames};
    use pods_pwrig::{DRIVER_NODE, LONGEST_RECORDING, Recorder, Server, node_names, write_wav};
    use rustix::process::{Pid, Signal, kill_process};

    /// The rate the player's file is written at.
    const PLAYER_RATE: u32 = 48_000;
    /// The rate Superwhisper records at.
    const SUPERWHISPER_RATE: u32 = 16_000;
    /// The rate of Superwhisper's second recorder.
    const HISTORY_RATE: u32 = 48_000;
    const TONE_HZ: f64 = 1_000.0;
    /// Half of full scale.
    const TONE_AMPLITUDE: f64 = 0.5;
    /// Longer than the lead, the settle and the checked span together.
    const TONE: Duration = Duration::from_secs(4);
    /// Recorded beyond the settle and the checked span, for the silence before the first sound.
    const LEAD: Duration = Duration::from_millis(200);
    const LEVEL_TOLERANCE_DB: f64 = 1.0;
    /// A local server answers in milliseconds; this is a hung one.
    const WITHIN: Duration = Duration::from_secs(5);
    /// The node name the stock player gives itself.
    const PLAYER: &str = "pw-play";
    /// The server's answer to a client that asks for its node, which the player writes to its standard error.
    const NO_CLIENT_NODE: &str = "unknown factory name client-node";
    /// A directory name long enough to put the socket path past 108 bytes on its own.
    const TOO_DEEP: usize = 120;
    /// The C library's text for ENAMETOOLONG, which pipewire writes to its log for that socket path.
    const NAME_TOO_LONG: &str = "File name too long";
    /// Past the 10.7 ms cycle the recorders ask for, where it dropped audio, and inside the rig's 42.7 ms cycle.
    const PLAYER_STALL: Duration = Duration::from_millis(30);
    /// Where the rig writes the server's standard error inside its directory.
    const SERVER_LOG: &str = "pipewire.log";
    /// The header pw-record writes, measured at 16 and 48 kHz.
    const WAV_HEADER_LEN: u64 = 44;
    /// The bytes of one sample in the recorders' s16 format.
    const BYTES_PER_SAMPLE: usize = 2;
    /// How long a test waits for something it expects never to come; a local server answers in milliseconds.
    const NEVER_WITHIN: Duration = Duration::from_millis(300);
    /// How often a wait in these tests looks again.
    const POLL: Duration = Duration::from_millis(10);

    #[test]
    fn the_server_starts_in_its_own_directory_and_stops() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let socket = server.socket();
        assert!(socket.starts_with(dir.path()), "{}", socket.display());
        assert!(socket.exists(), "no socket at {}", socket.display());
        assert_eq!(
            processes_holding(dir.path()).len(),
            1,
            "the server is one process"
        );
        server.stop();
        assert!(!socket.exists(), "{} remains after stop", socket.display());
        assert_eq!(processes_holding(dir.path()), Vec::<u32>::new());
    }

    #[test]
    fn a_tone_from_a_stock_player_reaches_superwhispers_recorder() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let recorder = Recorder::superwhisper(&server, PLAYER, &dir.path().join("capture.wav"));
        let _player = server.play(&tone(dir.path()));
        server.link(PLAYER, recorder.node());
        recorder.wait_frames(frames(LEAD + SETTLE + CHECKED, SUPERWHISPER_RATE), WITHIN);
        assert_whole(&recorder.finish(), SUPERWHISPER_RATE);
    }

    #[test]
    fn the_history_recorder_records_at_48_khz() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let capture = Recorder::superwhisper(&server, PLAYER, &dir.path().join("capture.wav"));
        let history = Recorder::history(&server, PLAYER, &dir.path().join("history.wav"));
        let _player = server.play(&tone(dir.path()));
        // Superwhisper runs both recorders at once, so the second link must leave the first standing.
        server.link(PLAYER, capture.node());
        server.link(PLAYER, history.node());
        history.wait_frames(frames(LEAD + SETTLE + CHECKED, HISTORY_RATE), WITHIN);
        capture.wait_frames(frames(LEAD + SETTLE + CHECKED, SUPERWHISPER_RATE), WITHIN);
        assert_server_logged_no_error(&server);
        assert_whole(&history.finish(), HISTORY_RATE);
        assert_whole(&capture.finish(), SUPERWHISPER_RATE);
    }

    #[test]
    fn a_player_a_loaded_host_holds_back_still_reaches_the_recorder() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let history = Recorder::history(&server, PLAYER, &dir.path().join("history.wav"));
        let _player = server.play(&tone(dir.path()));
        server.link(PLAYER, history.node());
        history.wait_frames(frames(LEAD + SETTLE, HISTORY_RATE), WITHIN);
        // A stopped child still dies by the SIGKILL of its drop, should a step below fail.
        let player = child_named(dir.path(), PLAYER);
        kill_process(player, Signal::STOP).unwrap();
        hold(PLAYER_STALL);
        kill_process(player, Signal::CONT).unwrap();
        history.wait_frames(frames(LEAD + SETTLE + CHECKED, HISTORY_RATE), WITHIN);
        assert_whole(&history.finish(), HISTORY_RATE);
    }

    #[test]
    fn restart_drops_every_node_and_serves_again() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let tone = tone(dir.path());
        let mut first = server.play(&tone);
        server.wait_node(PLAYER, WITHIN);
        server.restart();
        server.wait_no_node(PLAYER, WITHIN);
        let Some(status) = first.wait(WITHIN) else {
            panic!("the player outlived its server");
        };
        // Only the taken status keeps this failure from failing the player's drop.
        assert!(
            !status.success(),
            "pw-play exited cleanly from a server that went away"
        );
        assert_eq!(node_names(&server.dump()), [DRIVER_NODE]);
        let _second = server.play(&tone);
        server.wait_node(PLAYER, WITHIN);
    }

    #[test]
    fn a_server_without_client_nodes_refuses_a_player() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start_without_client_nodes(dir.path());
        let mut player = server.play(&tone(dir.path()));
        let Some(status) = player.wait(WITHIN) else {
            panic!("the player still runs on a server that cannot make its node");
        };
        assert!(!status.success(), "pw-play exited cleanly with no node");
        assert!(player.log().contains(NO_CLIENT_NODE), "{}", player.log());
        assert_eq!(node_names(&server.dump()), [DRIVER_NODE]);
    }

    #[test]
    fn the_private_server_has_no_device_node() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let _player = server.play(&tone(dir.path()));
        server.wait_node(PLAYER, WITHIN);
        let dump = server.dump();
        for object in dump.as_array().unwrap() {
            assert!(
                object["info"]["props"].get("device.api").is_none(),
                "a device on the private server: {object}"
            );
        }
        // Only what this test made, and the rig's driver: no node of the session's server is seen.
        assert_eq!(node_names(&dump), [DRIVER_NODE, PLAYER]);
    }

    #[test]
    fn children_get_the_private_directory_no_session_server_and_no_bus() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let command = server.command("pw-dump");
        let envs: Vec<(&OsStr, Option<&OsStr>)> = command.get_envs().collect();
        for name in ["XDG_RUNTIME_DIR", "XDG_CONFIG_HOME", "XDG_STATE_HOME"] {
            assert!(
                envs.contains(&(OsStr::new(name), Some(dir.path().as_os_str()))),
                "{name} is not the private directory: {envs:?}"
            );
        }
        for name in ["PIPEWIRE_REMOTE", "PIPEWIRE_RUNTIME_DIR", "PULSE_SERVER"] {
            assert!(
                envs.contains(&(OsStr::new(name), None)),
                "{name} is not removed: {envs:?}"
            );
        }
        let no_bus = format!("unix:path={}", dir.path().join("no-bus").display());
        assert!(!dir.path().join("no-bus").exists());
        for name in ["DBUS_SESSION_BUS_ADDRESS", "DBUS_SYSTEM_BUS_ADDRESS"] {
            assert!(
                envs.contains(&(OsStr::new(name), Some(OsStr::new(&no_bus)))),
                "{name} still names a bus: {envs:?}"
            );
        }
    }

    #[test]
    fn no_child_outlives_the_rig() {
        let dir = tempfile::tempdir().unwrap();
        let mut running = 0;
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            let server = Server::start(dir.path());
            let recorder = Recorder::superwhisper(&server, PLAYER, &dir.path().join("capture.wav"));
            let _player = server.play(&tone(dir.path()));
            server.link(PLAYER, recorder.node());
            recorder.wait_frames(1, WITHIN);
            running = processes_holding(dir.path()).len();
            panic!("a test that fails with the rig running");
        }));
        assert!(outcome.is_err());
        assert_eq!(running, 3, "the server, the recorder and the player ran");
        assert_eq!(processes_holding(dir.path()), Vec::<u32>::new());
    }

    #[test]
    #[should_panic(expected = "node `never-came` not seen within")]
    fn wait_node_names_the_node_that_never_came() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        server.wait_node("never-came", NEVER_WITHIN);
    }

    #[test]
    #[should_panic(expected = "`pw-play` exited with exit status: 1 on its own")]
    fn a_player_that_fails_on_its_own_fails_the_test() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let player = server.play(&tone(dir.path()));
        server.wait_node(PLAYER, WITHIN);
        server.stop();
        wait_until_none_hold(dir.path());
        drop(player);
    }

    #[test]
    #[should_panic(expected = "exited with exit status: 1 on its own, before")]
    fn a_recorder_that_ended_on_its_own_fails_its_finish() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let recorder = Recorder::superwhisper(&server, PLAYER, &dir.path().join("capture.wav"));
        server.wait_node(recorder.node(), WITHIN);
        server.stop();
        wait_until_none_hold(dir.path());
        drop(recorder.finish());
    }

    #[test]
    #[should_panic(expected = "pipewire ended with")]
    fn a_server_that_died_fails_its_stop() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        kill_server(dir.path());
        server.stop();
    }

    #[test]
    #[should_panic(expected = "pipewire.log: No such file or directory")]
    fn a_server_log_that_cannot_be_read_is_named_with_its_error() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        fs::remove_file(dir.path().join(SERVER_LOG)).unwrap();
        kill_server(dir.path());
        server.stop();
    }

    #[test]
    #[should_panic(expected = "superwhisper-capture holds 0 of 1 frames")]
    fn an_unlinked_recorder_holds_no_frame_however_long_its_header() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let file = dir.path().join("capture.wav");
        let recorder = Recorder::superwhisper(&server, PLAYER, &file);
        server.wait_node(recorder.node(), WITHIN);
        // pw-record writes its header before its node appears, so the wait below counts past a whole header.
        assert_eq!(fs::metadata(&file).unwrap().len(), WAV_HEADER_LEN);
        recorder.wait_frames(1, NEVER_WITHIN);
    }

    #[test]
    fn a_recording_past_the_limit_is_refused_with_its_path_and_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let file = dir.path().join("capture.wav");
        let recorder = Recorder::superwhisper(&server, PLAYER, &file);
        server.wait_node(recorder.node(), WITHIN);
        // An unlinked recorder writes no sample, so these bytes alone make the file that long.
        fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap()
            .write_all(&vec![0; recording_limit()])
            .unwrap();
        let Err(failure) = panic::catch_unwind(AssertUnwindSafe(|| recorder.finish())) else {
            panic!(
                "a recording past {} bytes was read whole",
                recording_limit()
            );
        };
        let message = failure.downcast_ref::<String>().unwrap();
        let refused = format!(
            "{} holds more than {} bytes",
            file.display(),
            recording_limit()
        );
        assert!(message.contains(&refused), "{message}");
    }

    #[test]
    #[should_panic(expected = "already exists, so its appearing cannot mean this server is ready")]
    fn a_second_server_in_one_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let _first = Server::start(dir.path());
        let _second = Server::start(dir.path());
    }

    #[test]
    fn a_server_that_cannot_start_quotes_its_log() {
        let dir = tempfile::tempdir().unwrap();
        // pipewire refuses a socket path longer than a socket address holds, 108 bytes.
        let deep = dir.path().join("d".repeat(TOO_DEEP));
        fs::create_dir(&deep).unwrap();
        let Err(failure) = panic::catch_unwind(|| Server::start(&deep)) else {
            panic!("a server started on a socket path past 108 bytes");
        };
        let message = failure.downcast_ref::<String>().unwrap();
        assert!(message.starts_with("pipewire exited with"), "{message}");
        assert!(message.contains(NAME_TOO_LONG), "{message}");
    }

    /// Writes the 1 kHz tone at half scale as a 48 kHz mono file in `dir`.
    fn tone(dir: &Path) -> PathBuf {
        let samples: Vec<i16> = (0..frames(TONE, PLAYER_RATE))
            .map(|n| {
                let seconds = f64::from(u32::try_from(n).unwrap()) / f64::from(PLAYER_RATE);
                to_sample((TAU * TONE_HZ * seconds).sin() * TONE_AMPLITUDE)
            })
            .collect();
        let path = dir.join("tone.wav");
        write_wav(&path, PLAYER_RATE, &samples);
        path
    }

    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "a value inside full scale, rounded to the nearest sample"
    )]
    fn to_sample(value: f64) -> i16 {
        (value * f64::from(i16::MAX)).round() as i16
    }

    fn assert_whole(samples: &[i16], rate: u32) {
        assert_tone(samples, rate, TONE_HZ, TONE_AMPLITUDE, LEVEL_TOLERANCE_DB);
    }

    /// The rig's limit for one recording: the longest recording at 48 kHz in 16 bit, plus the header.
    fn recording_limit() -> usize {
        frames(LONGEST_RECORDING, HISTORY_RATE)
            .checked_mul(BYTES_PER_SAMPLE)
            .and_then(|bytes| bytes.checked_add(usize::try_from(WAV_HEADER_LEN).unwrap()))
            .unwrap()
    }

    /// The one child of this test in `dir` whose command name is `name`.
    fn child_named(dir: &Path, name: &str) -> Pid {
        let named: Vec<u32> = processes_holding(dir)
            .into_iter()
            .filter(|pid| {
                // Sample input: `pw-play\n`; a process that ended meanwhile has no file.
                fs::read_to_string(format!("/proc/{pid}/comm"))
                    .is_ok_and(|comm| comm.trim_end() == name)
            })
            .collect();
        let [pid] = named[..] else {
            panic!("{} children named `{name}`", named.len());
        };
        Pid::from_raw(i32::try_from(pid).unwrap()).unwrap()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "std has no timed wait for a stopped child or a process scan, and these are tests"
    )]
    fn hold(span: Duration) {
        std::thread::sleep(span);
    }

    /// Kills the one server running in `dir` with SIGKILL, behind the rig's back.
    fn kill_server(dir: &Path) {
        let [pid] = processes_holding(dir)[..] else {
            panic!("the server is not one process");
        };
        let pid = Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
        kill_process(pid, Signal::KILL).unwrap();
    }

    /// Asserts the server logged no error, as it does when it refuses a request.
    fn assert_server_logged_no_error(server: &Server) {
        let log = server.log();
        // Sample input: `[E][17:33:45.315441] pw.core | [ impl-core.c: 221 core_error()] 0x55e6d33fad20: error -5 ...`.
        let errors: Vec<&str> = log.lines().filter(|line| line.starts_with("[E]")).collect();
        assert!(errors.is_empty(), "the server logged errors: {errors:#?}");
    }

    /// Waits until no child of this test runs with `dir` as its runtime directory, failing once `WITHIN` passes.
    fn wait_until_none_hold(dir: &Path) {
        let started = Instant::now();
        while !processes_holding(dir).is_empty() {
            assert!(started.elapsed() < WITHIN, "a child outlived its server");
            hold(POLL);
        }
    }

    /// The pids of this test's running children whose `XDG_RUNTIME_DIR` is `dir`, as every child of the rig has.
    fn processes_holding(dir: &Path) -> Vec<u32> {
        let parent = std::process::id().to_string();
        let mut variable = b"XDG_RUNTIME_DIR=".to_vec();
        variable.extend_from_slice(dir.as_os_str().as_encoded_bytes());
        let mut found = Vec::new();
        for entry in fs::read_dir("/proc").unwrap() {
            let entry = entry.unwrap();
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse().ok())
            else {
                continue;
            };
            // Sample input: a `PPid:\t4242` line of `/proc/<pid>/status`; a process that ended meanwhile has no file.
            let Ok(status) = fs::read_to_string(entry.path().join("status")) else {
                continue;
            };
            let ours = status
                .lines()
                .any(|line| line.strip_prefix("PPid:").map(str::trim) == Some(parent.as_str()));
            if !ours {
                continue;
            }
            // Sample input: `HOME=/home/user\0XDG_RUNTIME_DIR=/tmp/.tmpAb12Cd\0`; an exited child reads empty, and only children are read.
            let Ok(environment) = fs::read(entry.path().join("environ")) else {
                continue;
            };
            if environment
                .split(|&byte| byte == 0)
                .any(|entry| entry == variable.as_slice())
            {
                found.push(pid);
            }
        }
        found
    }
}
