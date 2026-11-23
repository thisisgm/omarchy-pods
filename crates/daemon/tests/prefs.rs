//! The ear detection choice and the notifications switch across restarts of the built daemon, the C++ daemon's old settings file read once, and the notice the switch silences.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use pods_fake::Profile;
    use pods_samples::{CASE, Cell};
    use serde_json::Value;

    use super::support::bus::{
        BUS_NAME, INTERFACE, Listener, OBJECT_PATH, changes_rule, start_bus, start_daemon,
    };
    use super::support::omarchy::{
        DISCONNECT_NOTICE, Mood, RECORDED_WITHIN, SLOW, low_battery_notice,
    };
    use super::support::rig::{Device, REAL_TIME_SLACK, Rig, assert_gap};
    use super::support::settings::{
        plant_settings, set, status, stop, unreadable_lines, wait_settings,
    };
    use super::support::{
        DAEMON_LOG, DIR_MODE, Daemon, FILE_MODE, HANG_LIMIT, Proto1, Session, binary, set_request,
        wait_for, wait_within,
    };

    const CANNOT_WRITE: &str = "prefs: cannot write";

    /// The daemon's pause between two tries of a save that failed.
    const RETRY_PAUSE: Duration = Duration::from_secs(2);

    /// Failed tries seen before the repair, so the one log line is seen to cover several.
    const FAILED_TRIES: usize = 2;

    /// Half the pause between tries: changes of the directory closer than this are one try's copy made and removed.
    const ONE_TRY: Duration = RETRY_PAUSE.checked_div(2).unwrap();

    /// The C++ daemon's file as Qt writes it: pause when both buds are out, notices off.
    const OLD_CONF: &str =
        "[General]\nphoneMac=\n\n[earDetection]\nsetting=1\n\n[notifications]\nenabled=false\n";

    /// The same file edited to say the opposite of both.
    const OLD_CONF_EDITED: &str = "[earDetection]\nsetting=2\n\n[notifications]\nenabled=true\n";

    /// The daemon's bound on the file, which a file of exactly this size is within.
    const FILE_MAX: usize = 65536;

    /// Settings the daemon would take, were the file read at all.
    const READABLE: &str = r#"{"version":1,"ear_detection":"both","notifications":false}"#;

    const BOTH_REMOVED: u64 = 1;

    const ONE_REMOVED: u64 = 0;

    const DISABLED: u64 = 2;

    /// A bud level at the low battery trigger, so both buds at it queue two notices at once.
    const LOW: u8 = 10;

    fn old_conf(session: &Session) -> PathBuf {
        session
            .config_home()
            .join("AirPodsTrayApp")
            .join("AirPodsTrayApp.conf")
    }

    fn write_old_conf(session: &Session, text: &str) {
        let path = old_conf(session);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// `READABLE` padded with trailing spaces, which JSON allows, to `len` bytes.
    fn padded(len: usize) -> String {
        let mut text = READABLE.to_owned();
        text.push_str(&" ".repeat(len.checked_sub(READABLE.len()).unwrap()));
        text
    }

    fn start_unmasked(session: &Session) -> Daemon {
        let mut command = session.unmasked_daemon();
        let daemon = session.spawn_command(&mut command, DAEMON_LOG);
        session.wait_accepting();
        daemon
    }

    /// The names in the settings directory, sorted; after a stop only `settings.json` belongs there.
    fn left_beside_settings(session: &Session) -> Vec<String> {
        let dir = session.settings_file().parent().unwrap().to_path_buf();
        let mut left: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        left
    }

    /// Waits for `tries` failed tries, each seen as changes of the settings directory as its copy is made and removed.
    fn wait_tries(session: &Session, tries: usize) {
        let dir = session.settings_file().parent().unwrap().to_path_buf();
        let changed = || {
            let found = fs::metadata(&dir).unwrap();
            (found.mtime(), found.mtime_nsec())
        };
        let mut last = changed();
        let mut counted: Option<Instant> = None;
        let mut seen: usize = 0;
        let within = RETRY_PAUSE
            .saturating_mul(u32::try_from(tries).unwrap())
            .saturating_add(REAL_TIME_SLACK);
        wait_within(&format!("{tries} failed tries"), within, || {
            let now = changed();
            let new_try = counted.is_none_or(|at| at.elapsed() >= ONE_TRY);
            if now != last && new_try {
                counted = Some(Instant::now());
                seen = seen.saturating_add(1);
            }
            last = now;
            (seen >= tries).then_some(())
        });
    }

    fn cannot_write_lines(daemon: &Daemon) -> usize {
        let log = daemon.log();
        log.lines()
            .filter(|line| line.starts_with(CANNOT_WRITE))
            .count()
    }

    fn assert_owner_only(file: &Path) {
        let mode = fs::metadata(file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, FILE_MODE);
        let dir_mode = fs::metadata(file.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(dir_mode & 0o777, DIR_MODE);
    }

    fn assert_prefs(session: &Session, ear_detection: u64, notifications: bool) {
        let status = status(session);
        assert_eq!(status["ear_detection_behavior"], ear_detection, "{status}");
        assert_eq!(status["notifications_enabled"], notifications, "{status}");
    }

    #[test]
    fn an_ear_detection_choice_survives_a_restart() {
        let session = Session::new();
        let mut first = session.start();
        set(&session, "ear_detection", "both");
        stop(&mut first);
        let mut second = session.start();
        assert_prefs(&session, BOTH_REMOVED, true);
        stop(&mut second);
    }

    #[test]
    fn notifications_off_survives_a_restart_and_the_stub_records_nothing() {
        let mut rig = Rig::new();
        let device = rig.add_pods(Profile::default());
        let mut first = rig.session().start();
        set(rig.session(), "notifications", "false");
        stop(&mut first);
        rig.start();
        assert_prefs(rig.session(), ONE_REMOVED, false);
        rig.link_up(&device);
        rig.wait_filled();
        rig.disconnect();
        rig.omarchy().never_ran(RECORDED_WITHIN);
        rig.finish();
    }

    #[test]
    fn notifications_back_on_and_the_next_loss_is_recorded() {
        let (rig, device) = Rig::with_linked_pods();
        set(rig.session(), "notifications", "false");
        rig.disconnect();
        rig.omarchy().never_ran(RECORDED_WITHIN);
        set(rig.session(), "notifications", "true");
        rig.relink(&device);
        rig.disconnect();
        let calls = rig.omarchy().wait_calls(1, HANG_LIMIT);
        assert_eq!(calls, [DISCONNECT_NOTICE]);
        rig.omarchy().none_for(RECORDED_WITHIN);
        rig.finish();
    }

    /// Both buds at the trigger with a slow stand-in, so the right bud's notice waits behind the left's run.
    fn start_two_notices() -> (Rig, Device) {
        let (rig, device) = Rig::with_linked_pods();
        rig.omarchy().become_(Mood::Slow);
        let low = Cell {
            level: LOW,
            charging: false,
        };
        device.pods.set_battery(low, low, CASE);
        let calls = rig.omarchy().wait_calls(1, HANG_LIMIT);
        assert_eq!(calls, [low_battery_notice("Left", LOW)]);
        (rig, device)
    }

    #[test]
    fn with_notices_on_the_queued_notice_runs_after_the_first() {
        let (rig, _device) = start_two_notices();
        let calls = rig.omarchy().wait_calls(2, HANG_LIMIT);
        let both = [
            low_battery_notice("Left", LOW),
            low_battery_notice("Right", LOW),
        ];
        assert_eq!(calls, both);
        rig.finish();
    }

    #[test]
    fn a_queued_notice_does_not_start_once_notices_are_off() {
        let (rig, _device) = start_two_notices();
        set(rig.session(), "notifications", "false");
        let calls = rig.omarchy().calls();
        assert_eq!(
            calls.len(),
            1,
            "the switch went off after the second run: {calls:?}"
        );
        rig.omarchy().none_for(SLOW.saturating_add(RECORDED_WITHIN));
        assert_eq!(rig.omarchy().calls().len(), 1);
        rig.finish();
    }

    #[test]
    fn status_carries_notifications_enabled_on_every_surface() {
        let session = Session::new();
        let bus = start_bus(&session, "bus");
        let mut daemon = start_daemon(&session, &bus.address, DAEMON_LOG);
        let listener = Listener::subscribe(&bus.address, &changes_rule());
        wait_for("the daemon to own its name", || {
            listener
                .ask_bus::<bool>("NameHasOwner", &(BUS_NAME,))
                .then_some(())
        });
        let mut client = Proto1::connect(&session);
        let reply = client.ask(&set_request(1, "notifications", "false"));
        assert_eq!(reply["result"], "confirmed", "{reply}");
        assert_surfaces(&session, &listener, false);
        let reply = listener.call(
            BUS_NAME,
            OBJECT_PATH,
            INTERFACE,
            "SetNotifications",
            &(true,),
        );
        let result: String = reply.unwrap().body().deserialize().unwrap();
        assert_eq!(result, "confirmed");
        assert_surfaces(&session, &listener, true);
        stop(&mut daemon);
    }

    /// The state file, `librepods status`, the legacy `status` verb and the D-Bus `StatusJson` each read `enabled`.
    fn assert_surfaces(session: &Session, listener: &Listener, enabled: bool) {
        wait_for("the state file to show the switch", || {
            let text = fs::read_to_string(session.state_file()).ok()?;
            let file: Value = serde_json::from_str(&text).ok()?;
            (file["notifications_enabled"] == enabled).then_some(())
        });
        let output = session.run_client(Path::new(binary()), &["status"]);
        let printed: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(printed["notifications_enabled"], enabled, "{printed}");
        assert_eq!(status(session)["notifications_enabled"], enabled);
        let json: Value = serde_json::from_str(&listener.property("StatusJson")).unwrap();
        assert_eq!(json["notifications_enabled"], enabled, "{json}");
    }

    #[test]
    fn the_old_file_is_read_once() {
        let session = Session::new();
        write_old_conf(&session, OLD_CONF);
        let mut first = session.start();
        assert_prefs(&session, BOTH_REMOVED, false);
        assert!(session.settings_file().is_file());
        assert_eq!(fs::read_to_string(old_conf(&session)).unwrap(), OLD_CONF);
        stop(&mut first);
        write_old_conf(&session, OLD_CONF_EDITED);
        let mut second = session.start();
        assert_prefs(&session, BOTH_REMOVED, false);
        stop(&mut second);
    }

    #[test]
    fn no_old_file_and_no_new_file_gives_the_defaults() {
        let session = Session::new();
        let mut daemon = session.start();
        assert_prefs(&session, ONE_REMOVED, true);
        assert!(!session.settings_file().exists());
        assert_eq!(unreadable_lines(&daemon), 0, "{}", daemon.log());
        stop(&mut daemon);
    }

    #[test]
    fn a_corrupt_settings_file_gives_defaults_and_one_log_line() {
        let session = Session::new();
        write_old_conf(&session, OLD_CONF);
        plant_settings(&session, &READABLE.as_bytes()[..READABLE.len() / 2]);
        let mut daemon = session.start();
        assert_prefs(&session, ONE_REMOVED, true);
        assert_eq!(unreadable_lines(&daemon), 1, "{}", daemon.log());
        set(&session, "ear_detection", "both");
        let settings = wait_settings(&session, "ear_detection", "both");
        assert_eq!(settings["notifications"], true, "{settings}");
        stop(&mut daemon);
    }

    #[test]
    fn a_file_of_exactly_the_bound_is_read() {
        let session = Session::new();
        plant_settings(&session, padded(FILE_MAX).as_bytes());
        let mut daemon = session.start();
        assert_prefs(&session, BOTH_REMOVED, false);
        assert_eq!(unreadable_lines(&daemon), 0, "{}", daemon.log());
        stop(&mut daemon);
    }

    #[test]
    fn an_oversized_file_is_not_read() {
        let session = Session::new();
        plant_settings(&session, padded(FILE_MAX + 1).as_bytes());
        let mut daemon = session.start();
        assert_prefs(&session, ONE_REMOVED, true);
        assert_eq!(unreadable_lines(&daemon), 1, "{}", daemon.log());
        stop(&mut daemon);
    }

    #[test]
    fn a_directory_in_place_of_the_file_does_not_stop_the_daemon() {
        let session = Session::new();
        fs::create_dir_all(session.settings_file()).unwrap();
        let mut daemon = session.start();
        assert_prefs(&session, ONE_REMOVED, true);
        assert_eq!(unreadable_lines(&daemon), 1, "{}", daemon.log());
        set(&session, "ear_detection", "both");
        assert_prefs(&session, BOTH_REMOVED, true);
        wait_for("the failed save in the log", || {
            let log = daemon.log();
            log.lines()
                .any(|line| line.starts_with(CANNOT_WRITE))
                .then_some(())
        });
        stop(&mut daemon);
        assert!(session.settings_file().is_dir());
        assert_eq!(left_beside_settings(&session), ["settings.json"]);
    }

    #[test]
    fn a_failed_save_is_retried_and_the_newest_value_lands() {
        let session = Session::new();
        fs::create_dir_all(session.settings_file()).unwrap();
        let mut daemon = session.start();
        set(&session, "ear_detection", "both");
        wait_for("the failed save in the log", || {
            (cannot_write_lines(&daemon) > 0).then_some(())
        });
        set(&session, "ear_detection", "off");
        wait_for("the state file to show the newer value", || {
            let text = fs::read_to_string(session.state_file()).ok()?;
            let status: Value = serde_json::from_str(&text).ok()?;
            (status["ear_detection_behavior"] == DISABLED).then_some(())
        });
        wait_tries(&session, FAILED_TRIES);
        assert_eq!(cannot_write_lines(&daemon), 1, "{}", daemon.log());
        fs::remove_dir(session.settings_file()).unwrap();
        let repaired = Instant::now();
        wait_for("the owed value in settings.json", || {
            let text = fs::read_to_string(session.settings_file()).ok()?;
            let settings: Value = serde_json::from_str(&text).ok()?;
            (settings["ear_detection"] == "off").then_some(())
        });
        let range = Duration::ZERO..=RETRY_PAUSE.saturating_add(REAL_TIME_SLACK);
        assert_gap(
            "the owed value after the repair",
            repaired,
            Instant::now(),
            &range,
        );
        assert_eq!(cannot_write_lines(&daemon), 1, "{}", daemon.log());
        stop(&mut daemon);
        let mut again = session.start();
        assert_prefs(&session, DISABLED, true);
        stop(&mut again);
    }

    #[test]
    fn the_file_and_its_directory_are_owner_only() {
        let session = Session::new();
        write_old_conf(&session, OLD_CONF);
        let mut daemon = start_unmasked(&session);
        // The migration wrote the file and made its directory before the runtime, so before the state file.
        assert_owner_only(&session.settings_file());
        set(&session, "ear_detection", "off");
        wait_settings(&session, "ear_detection", "off");
        assert_owner_only(&session.settings_file());
        stop(&mut daemon);
    }

    #[test]
    fn no_temporary_file_is_left() {
        let session = Session::new();
        let mut daemon = session.start();
        set(&session, "notifications", "false");
        set(&session, "ear_detection", "off");
        set(&session, "ear_detection", "both");
        let settings = wait_settings(&session, "ear_detection", "both");
        assert_eq!(settings["notifications"], false, "{settings}");
        stop(&mut daemon);
        assert_eq!(left_beside_settings(&session), ["settings.json"]);
    }

    #[test]
    fn set_notifications_maybe_is_bad_value() {
        let session = Session::new();
        let mut daemon = session.start();
        let output = session.run_client(Path::new(binary()), &["set", "notifications", "maybe"]);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            "That setting does not take this value.\nValues: true, false\n"
        );
        let mut client = Proto1::connect(&session);
        let reply = client.ask(&set_request(1, "notifications", "maybe"));
        assert_eq!(reply["error"]["code"], "bad_value", "{reply}");
        assert_prefs(&session, ONE_REMOVED, true);
        stop(&mut daemon);
    }
}
