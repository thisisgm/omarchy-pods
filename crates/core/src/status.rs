//! The one line of JSON the daemon publishes for the plugin and `librepods status`.

use std::collections::BTreeMap;

use pods_proto::battery::{Charge, Component};
use serde_json::{Map, Value};

use crate::state::State;

/// The document shape version; it changes only when a key changes meaning or goes away.
const SCHEMA_VERSION: u8 = 1;

/// The status document as compact JSON with a trailing newline, keys in ascending byte order like the C++ `QJsonObject`.
#[must_use]
pub fn to_status_line(state: &State) -> String {
    format!("{}\n", to_status_object(state))
}

/// The status document as a JSON object, which protocol 1 replies and events carry as their `state`.
#[must_use]
pub fn to_status_object(state: &State) -> Value {
    let counters = &state.counters;
    let capabilities = state.model.capabilities();
    let batteries = &state.batteries;
    object(BTreeMap::from([
        ("schema_version", Value::from(SCHEMA_VERSION)),
        ("connected", Value::from(state.connected)),
        ("device_name", Value::from(state.device_name.as_str())),
        ("noise_mode", Value::from(u8::from(state.noise_mode))),
        (
            "left",
            pod_object(batteries.left, in_ear(state, Component::Left)),
        ),
        (
            "right",
            pod_object(batteries.right, in_ear(state, Component::Right)),
        ),
        ("case", battery_object(batteries.case)),
        ("headset", battery_object(batteries.headset)),
        (
            "reconnect_attempts_total",
            Value::from(counters.reconnect_attempts),
        ),
        (
            "reconnect_failures_total",
            Value::from(counters.reconnect_failures),
        ),
        (
            "noise_control_changes_total",
            Value::from(counters.noise_control_changes),
        ),
        ("forget_calls_total", Value::from(counters.forget_calls)),
        (
            "ear_detection_changes_total",
            Value::from(counters.ear_detection_changes),
        ),
        (
            "ca_changes_total",
            Value::from(counters.conversational_awareness_changes),
        ),
        (
            "disconnect_calls_total",
            Value::from(counters.disconnect_calls),
        ),
        ("connect_calls_total", Value::from(counters.connect_calls)),
        (
            "disconnect_failures_total",
            Value::from(counters.disconnect_failures),
        ),
        (
            "connect_failures_total",
            Value::from(counters.connect_failures),
        ),
        (
            "adaptive_level_changes_total",
            Value::from(counters.adaptive_level_changes),
        ),
        (
            "one_bud_anc_changes_total",
            Value::from(counters.one_bud_anc_changes),
        ),
        ("reopen_calls_total", Value::from(counters.reopen_calls)),
        (
            "conversational_awareness",
            Value::from(state.conversational_awareness),
        ),
        (
            "adaptive_noise_level",
            Value::from(state.adaptive_noise_level),
        ),
        ("one_bud_anc_mode", Value::from(state.one_bud_anc_mode)),
        ("model_name", Value::from(state.model.display_name())),
        ("model_int", Value::from(state.model.as_int())),
        ("is_pro_series", Value::from(capabilities.is_pro_series)),
        ("is_headset", Value::from(capabilities.is_headset)),
        ("supports_noise_off", Value::from(capabilities.noise_off)),
        (
            "supports_noise_control",
            Value::from(capabilities.noise_control),
        ),
        ("supports_adaptive", Value::from(capabilities.adaptive)),
        (
            "supports_conversational_awareness",
            Value::from(capabilities.conversational_awareness),
        ),
        (
            "supports_one_bud_anc",
            Value::from(capabilities.one_bud_anc),
        ),
        ("model_number", Value::from(state.model_number.as_str())),
        (
            "ear_detection_behavior",
            Value::from(state.ear_detection_behavior.number()),
        ),
        ("lid_state", Value::from(state.lid_state.number())),
    ]))
}

/// A JSON object whose keys come out in the map's byte order, whatever `serde_json` features are on.
fn object(fields: BTreeMap<&'static str, Value>) -> Value {
    Value::Object(
        fields
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect::<Map<String, Value>>(),
    )
}

fn battery_fields(charge: Option<Charge>) -> BTreeMap<&'static str, Value> {
    let (available, charging, level) = battery_reading(charge);
    BTreeMap::from([
        ("available", Value::from(available)),
        ("level", Value::from(level)),
        ("charging", Value::from(charging)),
    ])
}

/// A part's battery as available, charging and level; with no reading it is not available, not charging, level 0, as the C++ `Battery` reset leaves it.
#[must_use]
pub fn battery_reading(charge: Option<Charge>) -> (bool, bool, u8) {
    charge.map_or((false, false, 0), |charge| {
        (true, charge.charging, charge.level)
    })
}

fn battery_object(charge: Option<Charge>) -> Value {
    object(battery_fields(charge))
}

fn pod_object(charge: Option<Charge>, in_ear: bool) -> Value {
    let mut fields = battery_fields(charge);
    fields.insert("in_ear", Value::from(in_ear));
    object(fields)
}

/// The ear report names primary and secondary; a side that is not the primary reads the secondary, as the C++ does.
#[must_use]
pub fn in_ear(state: &State, side: Component) -> bool {
    if state.primary == Some(side) {
        state.ear.primary_in_ear()
    } else {
        state.ear.secondary_in_ear()
    }
}

#[cfg(test)]
mod tests;
