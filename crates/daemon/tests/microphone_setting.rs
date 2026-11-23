//! The microphone preference through the built daemon: `set microphone` on every surface, the two status keys it moves, and `settings.json` across restarts.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::Path;

    use pods_core::prefs::PrefsError;
    use pods_core::status::{ADDITIVE_KEYS, CPP_KEY_COUNT};
    use pods_fake::Profile;
    use pods_proto::packets::REQUEST_PROXIMITY_KEYS;
    use pods_samples::{CONVERSATION_AWARENESS, MODEL_NUMBER, setting};
    use serde_json::Value;

    use super::support::bus::{
        BUS_NAME, INTERFACE, Listener, OBJECT_PATH, changes_rule, start_bus, start_daemon,
    };
    use super::support::rig::{Rig, connected};
    use super::support::settings::{
        UNREADABLE, plant_settings, set, status, stop, unreadable_lines, wait_settings,
    };
    use super::support::uplink::rig_with;
    use super::support::{
        DAEMON_LOG, HANG_LIMIT, Proto1, Session, binary, outcome, set_request, wait_for,
    };

    /// The line the C++ daemon wrote, which holds its 36 keys.
    const CPP_STATUS: &str = include_str!("../../core/tests/fixtures/status-disconnected.json");

    /// The two keys the preference adds to the status document.
    const NEW_KEYS: [&str; 2] = ["microphone", "mic"];

    /// Settings as the daemon wrote them before it kept the preference, both other choices off their defaults.
    const WITHOUT_THE_KEY: &str = r#"{"version":1,"ear_detection":"both","notifications":false}"#;

    const BAD_VALUE: &str = "io.github.thisisgm.Omapods1.Error.BadValue";

    /// The preference and what `mic` reads, as the status document holds them.
    fn microphone(status: &Value) -> (&str, &str) {
        NEW_KEYS.map(|key| status[key].as_str().unwrap()).into()
    }

    /// `settings.json` as a stopped daemon left it.
    fn kept(session: &Session) -> Value {
        serde_json::from_str(&fs::read_to_string(session.settings_file()).unwrap()).unwrap()
    }

    fn key_names(document: &Value) -> BTreeSet<&str> {
        let keys = document.as_object().unwrap().keys();
        keys.map(String::as_str).collect()
    }

    /// `document` without the keys `left_out` names.
    fn without(document: &Value, left_out: &[&str]) -> Value {
        let mut rest = document.clone();
        rest.as_object_mut()
            .unwrap()
            .retain(|key, _| !left_out.contains(&key.as_str()));
        rest
    }

    #[test]
    fn set_microphone_answers_at_the_write_and_survives_a_restart() {
        let session = Session::new();
        let mut first = session.start();
        let mut client = Proto1::connect(&session);
        let reply = client.ask(&set_request(1, "microphone", "always"));
        assert_eq!(reply["result"], "confirmed", "{reply}");
        assert_eq!(microphone(&status(&session)), ("always", "idle"));
        wait_settings(&session, "microphone", "always");
        stop(&mut first);
        let mut second = session.start();
        assert_eq!(microphone(&status(&session)), ("always", "idle"));
        stop(&mut second);
    }

    #[test]
    fn set_microphone_loud_is_bad_value() {
        let session = Session::new();
        let mut daemon = session.start();
        set(&session, "microphone", "always");
        let output = session.run_client(Path::new(binary()), &["set", "microphone", "loud"]);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            "That setting does not take this value.\nValues: auto, always, off\n"
        );
        let mut client = Proto1::connect(&session);
        for value in ["loud", "Off", "", "true"] {
            let reply = client.ask(&set_request(1, "microphone", value));
            assert_eq!(outcome(&reply), "bad_value", "{value:?}: {reply}");
        }
        assert_eq!(microphone(&status(&session)), ("always", "idle"));
        stop(&mut daemon);
    }

    #[test]
    fn a_settings_file_without_the_key_reads_auto() {
        let session = Session::new();
        plant_settings(&session, WITHOUT_THE_KEY.as_bytes());
        let mut daemon = session.start();
        let read = status(&session);
        assert_eq!(microphone(&read), ("auto", "idle"));
        assert_eq!(read["notifications_enabled"], false, "{read}");
        assert_eq!(unreadable_lines(&daemon), 0, "{}", daemon.log());
        set(&session, "notifications", "true");
        stop(&mut daemon);
        let settings = kept(&session);
        assert_eq!(settings["version"], 1, "{settings}");
        assert_eq!(settings["microphone"], "auto", "{settings}");
        assert_eq!(settings["ear_detection"], "both", "{settings}");
    }

    #[test]
    fn a_settings_file_with_a_bad_word_is_refused_like_any_bad_value() {
        for (text, reason) in [
            (
                r#"{"version":1,"ear_detection":"both","notifications":false,"microphone":"loud"}"#,
                PrefsError::Microphone,
            ),
            (
                r#"{"version":1,"ear_detection":"both","notifications":false,"microphone":true}"#,
                PrefsError::Microphone,
            ),
            (
                r#"{"version":1,"ear_detection":"two","notifications":false,"microphone":"off"}"#,
                PrefsError::EarDetection,
            ),
        ] {
            let line = format!("{UNREADABLE}: {reason}");
            let session = Session::new();
            plant_settings(&session, text.as_bytes());
            let mut daemon = session.start();
            let read = status(&session);
            assert_eq!(microphone(&read), ("auto", "idle"), "{text}");
            assert_eq!(read["notifications_enabled"], true, "{text}: {read}");
            let log = daemon.log();
            assert_eq!(unreadable_lines(&daemon), 1, "{text}: {log}");
            assert!(log.lines().any(|logged| logged == line), "{text}: {log}");
            stop(&mut daemon);
        }
    }

    #[test]
    fn the_preference_off_reads_off_and_any_other_word_reads_idle() {
        let session = Session::new();
        let mut first = session.start();
        assert_eq!(microphone(&status(&session)), ("auto", "idle"));
        for (word, mic) in [
            ("off", "off"),
            ("always", "idle"),
            ("off", "off"),
            ("auto", "idle"),
            ("off", "off"),
        ] {
            set(&session, "microphone", word);
            assert_eq!(microphone(&status(&session)), (word, mic));
        }
        stop(&mut first);
        let mut second = session.start();
        assert_eq!(microphone(&status(&session)), ("off", "off"));
        stop(&mut second);
    }

    #[test]
    fn the_new_keys_are_additive_and_the_36_are_unchanged() {
        let session = Session::new();
        let mut daemon = session.start();
        let before = status(&session);
        for key in NEW_KEYS {
            assert!(ADDITIVE_KEYS.contains(&key), "{key}");
            assert!(before.get(key).is_some(), "{key}: {before}");
        }
        let cpp: Value = serde_json::from_str(CPP_STATUS).unwrap();
        assert_eq!(key_names(&cpp).len(), CPP_KEY_COUNT);
        assert_eq!(key_names(&without(&before, ADDITIVE_KEYS)), key_names(&cpp));
        set(&session, "microphone", "off");
        let after = status(&session);
        assert_eq!(microphone(&after), ("off", "off"));
        assert_eq!(without(&after, &NEW_KEYS), without(&before, &NEW_KEYS));
        let output = session.run_client(Path::new(binary()), &["status"]);
        let printed: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(printed, after);
        wait_for("the state file to show the preference", || {
            let text = fs::read_to_string(session.state_file()).ok()?;
            let file: Value = serde_json::from_str(&text).ok()?;
            (file == after).then_some(())
        });
        stop(&mut daemon);
    }

    #[test]
    fn the_dbus_property_follows_the_setting() {
        let session = Session::new();
        let bus = start_bus(&session, "bus");
        let mut daemon = start_daemon(&session, &bus.address, DAEMON_LOG);
        let mut listener = Listener::subscribe(&bus.address, &changes_rule());
        wait_for("the daemon to own its name", || {
            listener
                .ask_bus::<bool>("NameHasOwner", &(BUS_NAME,))
                .then_some(())
        });
        let shown = |listener: &Listener| {
            let json: Value = serde_json::from_str(&listener.property("StatusJson")).unwrap();
            let properties = (listener.property("Microphone"), listener.property("Mic"));
            assert_eq!(microphone(&json), (&*properties.0, &*properties.1));
            properties
        };
        assert_eq!(shown(&listener), ("auto".to_owned(), "idle".to_owned()));
        set(&session, "microphone", "off");
        let changed = listener.next_change();
        let names: BTreeSet<&str> = changed.keys().map(String::as_str).collect();
        assert_eq!(names, BTreeSet::from(["Mic", "Microphone", "StatusJson"]));
        assert_eq!(shown(&listener), ("off".to_owned(), "off".to_owned()));
        let call =
            |word: &str| listener.call(BUS_NAME, OBJECT_PATH, INTERFACE, "SetMicrophone", &(word,));
        let result: String = call("always").unwrap().body().deserialize().unwrap();
        assert_eq!(result, "confirmed");
        assert_eq!(shown(&listener), ("always".to_owned(), "idle".to_owned()));
        let Err(zbus::Error::MethodError(name, ..)) = call("loud") else {
            panic!("SetMicrophone took a word outside the three");
        };
        assert_eq!(name.as_str(), BAD_VALUE);
        assert_eq!(shown(&listener), ("always".to_owned(), "idle".to_owned()));
        stop(&mut daemon);
    }

    #[test]
    fn set_microphone_works_with_no_link() {
        let (rig, _device) = Rig::with_known_pods();
        let mut client = Proto1::connect(rig.session());
        let reply = client.ask(&set_request(1, "microphone", "off"));
        assert_eq!(outcome(&reply), "confirmed", "{reply}");
        let read = rig.status();
        assert!(!connected(&read), "{read}");
        assert_eq!(microphone(&read), ("off", "off"));
        rig.finish();
    }

    #[test]
    fn set_microphone_with_the_link_up_writes_nothing_and_outlives_the_device() {
        // A confirmed model under `auto` sends no START, so the key request stays the last write of the burst.
        let (rig, device) = rig_with("auto", &[MODEL_NUMBER], Profile::default());
        rig.link_up(&device);
        rig.wait_filled();
        let pods = &device.pods;
        // The daemon's last write of its own after the link comes up, so nothing of the burst is still on its way.
        pods.wait_heard(&REQUEST_PROXIMITY_KEYS, 1, HANG_LIMIT)
            .unwrap_or_else(|_| panic!("no keys request heard"));
        let heard_before = pods.heard().len();
        let mut client = Proto1::connect(rig.session());
        let reply = client.ask(&set_request(1, "microphone", "off"));
        assert_eq!(outcome(&reply), "confirmed", "{reply}");
        let read = rig.status();
        assert!(connected(&read), "{read}");
        assert_eq!(microphone(&read), ("off", "off"));
        // A switch written after it is heard after anything written before it.
        let switch = setting(CONVERSATION_AWARENESS, 1);
        let reply = client.ask(&set_request(2, "conversational_awareness", "true"));
        assert_eq!(outcome(&reply), "sent", "{reply}");
        pods.wait_heard(&switch, 1, HANG_LIMIT)
            .unwrap_or_else(|_| panic!("no switch frame heard"));
        let since: Vec<Vec<u8>> = pods
            .heard()
            .into_iter()
            .skip(heard_before)
            .map(|heard| heard.sdu)
            .collect();
        assert_eq!(since, [switch]);
        rig.disconnect();
        assert_eq!(microphone(&rig.status()), ("off", "off"));
        rig.finish();
    }
}
