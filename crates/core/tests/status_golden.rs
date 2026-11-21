//! The status line against the one the C++ 1.3.6 daemon wrote with no pods connected.

use pods_core::state::{Counters, LidState, State};
use pods_core::status::to_status_line;
use serde_json::Value;

/// The state the C++ daemon was in when the fixture was captured.
fn fixture_state() -> State {
    State {
        model_number: "A3063".to_owned(),
        lid_state: LidState::Unknown,
        counters: Counters {
            noise_control_changes: 1,
            reconnect_failures: 1,
            ..Counters::default()
        },
        ..State::default()
    }
}

/// The keys of the outermost object, in the order the line writes them.
// Sample input: {"a":1,"b":{"c":"x:y"},"d":[{"e":0}]} gives a, b, d.
fn top_level_keys(line: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut depth: u32 = 0;
    let mut in_string = false;
    let mut escaped = false;
    let mut text = String::new();
    let mut last_string = String::new();
    for c in line.chars() {
        if in_string {
            if escaped {
                escaped = false;
                text.push(c);
            } else if c == '\\' {
                escaped = true;
                text.push(c);
            } else if c == '"' {
                in_string = false;
                last_string = std::mem::take(&mut text);
            } else {
                text.push(c);
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' | '[' => depth = depth.saturating_add(1),
            '}' | ']' => depth = depth.saturating_sub(1),
            ':' if depth == 1 => keys.push(std::mem::take(&mut last_string)),
            _ => {}
        }
    }
    keys
}

#[test]
fn disconnected_status_line_matches_the_cpp_daemon_byte_for_byte() {
    let golden = include_str!("fixtures/status-disconnected.json");
    assert_eq!(to_status_line(&fixture_state()), golden);
}

#[test]
fn status_keys_are_in_ascending_byte_order() {
    let keys = top_level_keys(&to_status_line(&fixture_state()));
    assert_eq!(keys.len(), 36, "{keys:?}");
    let mut sorted = keys.clone();
    sorted.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    sorted.dedup();
    assert_eq!(keys, sorted);
}

#[test]
fn top_level_keys_sees_an_out_of_order_line() {
    let keys = top_level_keys(r#"{"b":{"a":"x:y"},"a":[{"c":0}]}"#);
    assert_eq!(keys, ["b", "a"]);
}

#[test]
fn default_state_has_all_four_battery_objects() {
    let status: Value = serde_json::from_str(&to_status_line(&State::default())).unwrap();
    for part in ["left", "right", "case", "headset"] {
        let object = &status[part];
        assert_eq!(object["available"], Value::Bool(false), "{part}");
        assert_eq!(object["level"], Value::from(0), "{part}");
        assert_eq!(object["charging"], Value::Bool(false), "{part}");
    }
}
