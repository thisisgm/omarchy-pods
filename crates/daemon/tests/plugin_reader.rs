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

    use pods_core::state::{EarDetectionBehavior, LidState, State};
    use pods_core::status::to_status_object;
    use pods_proto::noise::NoiseMode;
    use serde_json::Value;

    use super::support::{Session, binary, wait_for};

    const MODEL_JS: &str = include_str!("../../../Model.js");

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

    /// The string array inside `function name`: `var verbs = ["ear:one", "ear:both"]` gives both verbs.
    fn strings_in_function(text: &'static str, name: &str) -> Vec<&'static str> {
        let list = text
            .split_once(&format!("function {name}("))
            .and_then(|(_, body)| body.split_once('['))
            .and_then(|(_, rest)| rest.split_once(']'))
            .unwrap_or_else(|| panic!("Model.js: no array literal in `function {name}`"))
            .0;
        let strings: Vec<_> = list
            .split(',')
            .map(|item| item.trim().trim_matches('"'))
            .collect();
        assert!(
            strings.iter().all(|item| !item.is_empty()),
            "Model.js: `function {name}` has an empty item in `[{list}]`"
        );
        strings
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
        let panel_verbs = strings_in_function(MODEL_JS, "earDetectionVerb");
        let mut numbers = BTreeMap::new();
        for behavior in EAR_BEHAVIORS {
            let (constant, verb) = ear_constant_and_verb(behavior);
            let number = block
                .get(constant)
                .unwrap_or_else(|| panic!("Model.js has no `{constant}` in {block:?}"));
            let sent = usize::try_from(*number)
                .ok()
                .and_then(|index| panel_verbs.get(index));
            assert_eq!(sent, Some(&verb), "the verb Model.js sends for {constant}");
            let out = session.run_client(Path::new(binary()), &["ctl", verb]);
            assert!(out.status.success(), "{verb}: {out:?}");
            let document = settled_state_file(&session);
            numbers.insert(constant, number_at(&document, "ear_detection_behavior"));
        }
        assert_eq!(block, numbers);
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
    fn strings_in_function_reads_the_first_array_of_that_function() {
        let text =
            r#"function a() { var v = ["x"] } function b(n) { var v = ["y", "z"]; return v[n] }"#;
        assert_eq!(strings_in_function(text, "b"), ["y", "z"]);
    }

    #[test]
    #[should_panic(expected = "Model.js has no `var LID_` block")]
    fn a_text_with_no_block_fails() {
        constant_block("var NOISE_OFF = 0", "LID_");
    }
}
