//! The plugin's `Model.js`, read as text, against the status document the daemon writes.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "these tests read the state file and send verbs, so the signal and log helpers the other suites share go unused"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::Path;

    use pods_core::command::CommandError;
    use pods_core::state::{AdvertIntake, EarDetectionBehavior, LidState, State};
    use pods_core::status::to_status_object;
    use pods_fake::Profile;
    use pods_proto::noise::NoiseMode;
    use serde_json::Value;

    use super::support::rig::Rig;
    use super::support::{Proto1, Session, binary, outcome, set_request, wait_for};

    const MODEL_JS: &str = include_str!("../../../Model.js");

    /// The document the C++ daemon wrote with nothing connected.
    const CPP_STATUS: &str = include_str!("../../core/tests/fixtures/status-disconnected.json");

    /// A Pro 2 offers Off, Adaptive and both switches, where the default Pro 3 has no Off.
    const EVERY_SETTING_MODEL: &str = "A3047";

    /// The level the adaptive change is tried with, in percent.
    const LEVEL: &str = "40";

    const NOISE_MODES: [NoiseMode; 4] = [
        NoiseMode::Off,
        NoiseMode::Anc,
        NoiseMode::Transparency,
        NoiseMode::Adaptive,
    ];

    const LID_STATES: [LidState; 3] = [LidState::Open, LidState::Closed, LidState::Unknown];

    const EAR_BEHAVIORS: [EarDetectionBehavior; 3] = [
        EarDetectionBehavior::PauseWhenBothRemoved,
        EarDetectionBehavior::Disabled,
        EarDetectionBehavior::PauseWhenOneRemoved,
    ];

    fn noise_constant(mode: NoiseMode) -> &'static str {
        match mode {
            NoiseMode::Off => "NOISE_OFF",
            NoiseMode::Anc => "NOISE_ANC",
            NoiseMode::Transparency => "NOISE_TRANSPARENCY",
            NoiseMode::Adaptive => "NOISE_ADAPTIVE",
        }
    }

    fn lid_constant(lid: LidState) -> &'static str {
        match lid {
            LidState::Open => "LID_OPEN",
            LidState::Closed => "LID_CLOSED",
            LidState::Unknown => "LID_UNKNOWN",
        }
    }

    /// The constant for `behavior` and the verb that chooses it.
    fn ear_constant_and_verb(behavior: EarDetectionBehavior) -> (&'static str, &'static str) {
        match behavior {
            EarDetectionBehavior::PauseWhenOneRemoved => ("EAR_PAUSE_ONE_OUT", "ear:one"),
            EarDetectionBehavior::PauseWhenBothRemoved => ("EAR_PAUSE_BOTH_OUT", "ear:both"),
            EarDetectionBehavior::Disabled => ("EAR_DISABLED", "ear:off"),
        }
    }

    fn is_identifier_char(c: char) -> bool {
        c.is_ascii_alphanumeric() || c == '_' || c == '$'
    }

    /// The case and the headset have no ear sensor, so podFrom reads `in_ear` off them as false.
    fn absent_by_design(part: &str, key: &str) -> bool {
        key == "in_ear" && matches!(part, "case" | "headset")
    }

    /// Every key `text` reads off `object`: `x = parsed.connected; podFrom(parsed["case"])` gives connected and case.
    fn keys_read_off(text: &'static str, object: &str) -> BTreeSet<&'static str> {
        let mut keys = BTreeSet::new();
        for (at, _) in text.match_indices(object) {
            let before = text.get(..at).unwrap_or_default();
            if before.chars().next_back().is_some_and(is_identifier_char) {
                continue;
            }
            let after = text
                .get(at..)
                .and_then(|rest| rest.strip_prefix(object))
                .unwrap_or_default();
            let key = if let Some(rest) = after.strip_prefix('.') {
                rest.split(|c| !is_identifier_char(c))
                    .next()
                    .unwrap_or_default()
            } else if let Some(rest) = after.strip_prefix("[\"") {
                rest.split_once('"')
                    .unwrap_or_else(|| panic!("Model.js: `{object}[\"` with no closing quote"))
                    .0
            } else {
                continue;
            };
            keys.insert(key);
        }
        assert!(!keys.is_empty(), "Model.js reads no key off `{object}`");
        keys
    }

    /// The first run of `var` lines whose names start with `prefix`: `var LID_OPEN = 0` and `var LID_CLOSED = 1` give both.
    fn constant_block(text: &'static str, prefix: &str) -> BTreeMap<&'static str, i64> {
        let start = format!("var {prefix}");
        let block: BTreeMap<_, _> = text
            .lines()
            .skip_while(|line| !line.starts_with(&start))
            .take_while(|line| line.starts_with(&start))
            .map(|line| {
                let (name, value) = line
                    .strip_prefix("var ")
                    .and_then(|rest| rest.split_once(" = "))
                    .unwrap_or_else(|| panic!("Model.js: `{line}` is not `var NAME = N`"));
                let value = value
                    .parse()
                    .unwrap_or_else(|_| panic!("Model.js: `{line}` has no integer value"));
                (name, value)
            })
            .collect();
        assert!(!block.is_empty(), "Model.js has no `{start}` block");
        block
    }

    /// The text after `function name(` up to the `}` that starts a line: `function a(n) {\n  return n\n}` gives `n) {\n  return n`.
    fn function_body(text: &'static str, name: &str) -> &'static str {
        text.split_once(&format!("function {name}("))
            .and_then(|(_, rest)| rest.split_once("\n}"))
            .unwrap_or_else(|| panic!("Model.js: no `function {name}`"))
            .0
    }

    /// The strings of `var list` inside `function name`: `var values = ["one", "both"]` gives both.
    fn named_strings(text: &'static str, name: &str, list: &str) -> Vec<&'static str> {
        let body = function_body(text, name);
        let items = body
            .split_once(&format!("var {list} = ["))
            .and_then(|(_, rest)| rest.split_once(']'))
            .unwrap_or_else(|| panic!("Model.js: no `var {list} = [` in `function {name}`"))
            .0;
        let strings: Vec<_> = items
            .split(',')
            .map(|item| item.trim().trim_matches('"'))
            .collect();
        assert!(
            strings.iter().all(|item| !item.is_empty()),
            "Model.js: `function {name}` has an empty item in `[{items}]`"
        );
        strings
    }

    /// The quoted strings of the `return changeOf(` line of `function name`: `return changeOf("ear_detection", values[at], "ear:" + values[at])` gives `ear_detection` and `ear:`.
    fn change_strings(text: &'static str, name: &str) -> Vec<&'static str> {
        let line = function_body(text, name)
            .lines()
            .find(|line| line.trim_start().starts_with("return changeOf("))
            .unwrap_or_else(|| panic!("Model.js: no `return changeOf(` in `function {name}`"));
        line.split('"').skip(1).step_by(2).collect()
    }

    /// The string `var name` holds: `var CURRENT_DAEMON_KEY = "advert_intake"` gives `advert_intake`.
    fn var_string(text: &'static str, name: &str) -> &'static str {
        text.lines()
            .find_map(|line| line.strip_prefix(&format!("var {name} = \"")))
            .and_then(|rest| rest.strip_suffix('"'))
            .unwrap_or_else(|| panic!("Model.js: no `var {name} = \"...\"`"))
    }

    /// The key a change function's `return changeOf(` line names.
    fn change_key(name: &str) -> &'static str {
        change_strings(MODEL_JS, name)[0]
    }

    /// Every key and value the panel can send, the level after the modes because the pods take one only in Adaptive.
    fn panel_sets() -> Vec<(&'static str, &'static str)> {
        assert_eq!(
            named_strings(MODEL_JS, "noiseChange", "values").last(),
            Some(&"adaptive"),
            "Model.js: Adaptive is not the last mode, so the level would be set outside it"
        );
        let mut sets = Vec::new();
        for function in ["noiseChange", "earChange"] {
            let key = change_key(function);
            sets.extend(
                named_strings(MODEL_JS, function, "values")
                    .into_iter()
                    .map(|value| (key, value)),
            );
        }
        sets.push((change_key("adaptiveChange"), LEVEL));
        for key in named_strings(MODEL_JS, "switchChange", "keys") {
            for value in named_strings(MODEL_JS, "switchChange", "values") {
                sets.push((key, value));
            }
        }
        sets
    }

    fn number_at(document: &Value, key: &str) -> i64 {
        document[key]
            .as_i64()
            .unwrap_or_else(|| panic!("no integer `{key}` in {document}"))
    }

    fn written(state: &State, key: &str) -> i64 {
        number_at(&to_status_object(state), key)
    }

    /// The state file once it carries what the socket reports, so a change just made has landed in it.
    fn settled_state_file(session: &Session) -> Value {
        let status = session.status();
        let file = wait_for("the state file to match the status reply", || {
            fs::read_to_string(session.state_file())
                .ok()
                .filter(|file| *file == status)
        });
        serde_json::from_str(&file).unwrap()
    }

    #[test]
    fn every_key_the_plugin_reads_is_in_the_state_file() {
        let session = Session::new();
        let _daemon = session.start();
        let document = settled_state_file(&session);
        let top = document.as_object().unwrap();
        let missing: Vec<_> = keys_read_off(MODEL_JS, "parsed")
            .into_iter()
            .filter(|key| !top.contains_key(*key))
            .collect();
        assert!(
            missing.is_empty(),
            "the state file lacks {missing:?}: {document}"
        );
        let read_off_a_part = keys_read_off(MODEL_JS, "raw");
        for part in keys_read_off(MODEL_JS, "podFrom(parsed") {
            let object = document[part]
                .as_object()
                .unwrap_or_else(|| panic!("`{part}` is not an object: {document}"));
            let missing: Vec<_> = read_off_a_part
                .iter()
                .filter(|key| !object.contains_key(**key) && !absent_by_design(part, key))
                .collect();
            assert!(missing.is_empty(), "`{part}` lacks {missing:?}: {document}");
        }
    }

    #[test]
    fn ear_detection_numbers_are_the_state_file_numbers_after_each_verb() {
        let session = Session::new();
        let _daemon = session.start();
        let block = constant_block(MODEL_JS, "EAR_");
        let prefix = change_strings(MODEL_JS, "earChange")[1];
        let panel_verbs: Vec<_> = named_strings(MODEL_JS, "earChange", "values")
            .into_iter()
            .map(|value| format!("{prefix}{value}"))
            .collect();
        let mut numbers = BTreeMap::new();
        for behavior in EAR_BEHAVIORS {
            let (constant, verb) = ear_constant_and_verb(behavior);
            let number = block
                .get(constant)
                .unwrap_or_else(|| panic!("Model.js has no `{constant}` in {block:?}"));
            let sent = usize::try_from(*number)
                .ok()
                .and_then(|index| panel_verbs.get(index));
            assert_eq!(
                sent.map(String::as_str),
                Some(verb),
                "the verb Model.js sends for {constant}"
            );
            let out = session.run_client(Path::new(binary()), &["ctl", verb]);
            assert!(out.status.success(), "{verb}: {out:?}");
            let document = settled_state_file(&session);
            numbers.insert(constant, number_at(&document, "ear_detection_behavior"));
        }
        assert_eq!(block, numbers);
    }

    #[test]
    fn every_change_the_panel_can_make_is_a_setting_the_daemon_takes() {
        let mut rig = Rig::new();
        let device = rig.add_pods(Profile {
            model_number: EVERY_SETTING_MODEL.to_owned(),
            ..Profile::default()
        });
        rig.start();
        rig.link_up(&device);
        rig.wait_filled();
        for (key, value) in panel_sets() {
            let output = rig.client(&["set", key, value]);
            assert_eq!(
                output.status.code(),
                Some(0),
                "set {key} {value}: {output:?}"
            );
        }
        rig.finish();
    }

    #[test]
    fn a_refused_change_exits_non_zero_with_one_sentence() {
        let session = Session::new();
        let _daemon = session.start();
        let mut socket = Proto1::connect(&session);
        let mut refused = 0;
        for (id, (key, value)) in panel_sets().into_iter().enumerate() {
            let answer = socket.ask(&set_request(id, key, value));
            if answer["result"].is_string() {
                continue;
            }
            let code = outcome(&answer);
            let error = CommandError::ALL
                .into_iter()
                .find(|error| error.code() == code)
                .unwrap_or_else(|| panic!("set {key} {value}: a code no error has: {answer}"));
            let output = session.run_client(Path::new(binary()), &["set", key, value]);
            assert!(!output.status.success(), "set {key} {value}: {output:?}");
            assert_eq!(
                String::from_utf8(output.stderr).unwrap(),
                format!("{}\n", error.message()),
                "set {key} {value}"
            );
            refused += 1;
        }
        assert!(refused > 0, "with no device the daemon refused no change");
    }

    #[test]
    fn the_document_always_holds_the_key_that_marks_the_daemon_current() {
        let key = var_string(MODEL_JS, "CURRENT_DAEMON_KEY");
        let cpp: Value = serde_json::from_str(CPP_STATUS).unwrap();
        assert!(
            cpp.get(key).is_none(),
            "the C++ daemon's document holds {key}"
        );
        let mut rig = Rig::new();
        let device = rig.add_pods(Profile::default());
        rig.start();
        let before = settled_state_file(rig.session());
        assert!(
            before.get(key).is_some(),
            "no {key} before the link: {before}"
        );
        rig.link_up(&device);
        rig.wait_filled();
        let linked = settled_state_file(rig.session());
        assert!(
            linked.get(key).is_some(),
            "no {key} with the link up: {linked}"
        );
        rig.finish();
    }

    #[test]
    fn a_daemon_whose_bluez_has_no_monitor_writes_unavailable_and_the_panel_reads_it() {
        let word = AdvertIntake::Unavailable.word();
        assert_eq!(var_string(MODEL_JS, "ADVERT_UNAVAILABLE"), word);
        assert!(
            function_body(MODEL_JS, "advertHint").contains("=== ADVERT_UNAVAILABLE"),
            "Model.js: advertHint does not compare with ADVERT_UNAVAILABLE"
        );
        let mut rig = Rig::new();
        rig.session().store_sample_keys(None);
        rig.start();
        rig.wait_status(&format!("advert_intake {word}"), |status| {
            status["advert_intake"] == word
        });
        let document = settled_state_file(rig.session());
        assert_eq!(document["advert_intake"], word, "{document}");
        assert_eq!(document["connected"], false, "{document}");
        rig.session().keep_state("bluez-off");
        rig.finish();
    }

    #[test]
    fn noise_and_lid_numbers_are_the_document_numbers() {
        // No user path yet: with no link the daemon stores no mode and no lid state, so these go through the serializer it writes with.
        let noise = NOISE_MODES.map(|noise_mode| {
            let state = State {
                noise_mode,
                ..State::default()
            };
            (noise_constant(noise_mode), written(&state, "noise_mode"))
        });
        assert_eq!(constant_block(MODEL_JS, "NOISE_"), BTreeMap::from(noise));
        let lid = LID_STATES.map(|lid_state| {
            let state = State {
                lid_state,
                ..State::default()
            };
            (lid_constant(lid_state), written(&state, "lid_state"))
        });
        assert_eq!(constant_block(MODEL_JS, "LID_"), BTreeMap::from(lid));
    }

    #[test]
    fn keys_read_off_sees_both_forms_and_skips_longer_names() {
        let text = r#"a = parsed.left; b = parsed["case"]; c = unparsed.x; d = parsed"#;
        assert_eq!(
            keys_read_off(text, "parsed"),
            BTreeSet::from(["case", "left"])
        );
    }

    #[test]
    #[should_panic(expected = "Model.js reads no key off `parsed`")]
    fn a_text_with_no_reads_fails() {
        keys_read_off("var parsed = JSON.parse(text)", "parsed");
    }

    #[test]
    fn the_change_readers_take_their_own_function_only() {
        let text = "function a(n) {\n  var values = [\"x\"]\n  return changeOf(\"k\", values[n], \"k:\" + values[n])\n}\nfunction b(n) {\n  var values = [\"y\", \"z\"]\n  return changeOf(\"j\", values[n], \"j:\" + values[n])\n}";
        assert_eq!(named_strings(text, "b", "values"), ["y", "z"]);
        assert_eq!(change_strings(text, "b"), ["j", "j:"]);
    }

    #[test]
    #[should_panic(expected = "Model.js: no `var values = [` in `function a`")]
    fn a_change_reader_never_reads_past_its_own_function() {
        named_strings(
            "function a(n) {\n  return n\n}\nfunction b(n) {\n  var values = [\"y\"]\n}",
            "a",
            "values",
        );
    }

    #[test]
    #[should_panic(expected = "Model.js: `function a` has an empty item in")]
    fn a_list_with_an_empty_item_fails() {
        named_strings("function a(n) {\n  var values = [\"x\",]\n}", "a", "values");
    }

    #[test]
    #[should_panic(expected = "Model.js has no `var LID_` block")]
    fn a_text_with_no_block_fails() {
        constant_block("var NOISE_OFF = 0", "LID_");
    }
}
