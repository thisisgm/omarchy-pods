//! The one line of JSON the daemon publishes for the plugin and `librepods status`.

use std::collections::BTreeMap;

use pods_proto::battery::{Charge, Component};
use serde_json::{Map, Value};

use crate::state::{Counters, State};

/// Changes only when a key changes meaning or goes away.
const SCHEMA_VERSION: u8 = 1;

/// How many keys the C++ daemon writes, as its golden status line holds them.
pub const CPP_KEY_COUNT: usize = 36;

/// The keys this daemon adds to the `CPP_KEY_COUNT` the C++ daemon writes.
#[rustfmt::skip]
pub const ADDITIVE_KEYS: &[&str] = &[
    "notifications_enabled",
    "audio_server",
    "advert_intake",
    "microphone",
    "mic",
    "mic_dictation",
    "mic_start_failures",
    "mic_source",
    "mic_underruns",
    "mic_concealed",
    "mic_decode_errors",
    "mic_limited",
    "mic_nonfinite",
];

/// Keys in ascending byte order, as the C++ `QJsonObject` writes them.
#[must_use]
pub fn to_status_line(state: &State) -> String {
    format!("{}\n", to_status_object(state))
}

/// What protocol 1 replies and events carry as their `state`.
#[must_use]
pub fn to_status_object(state: &State) -> Value {
    let capabilities = state.capabilities();
    let batteries = &state.batteries;
    let counts = state.microphone.counts();
    let mut fields = BTreeMap::from([
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
            "conversational_awareness",
            Value::from(state.conversational_awareness),
        ),
        (
            "adaptive_noise_level",
            Value::from(state.adaptive_noise_level.percent()),
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
        ("notifications_enabled", Value::from(state.notifications)),
        ("audio_server", Value::from(state.audio_server.word())),
        ("advert_intake", Value::from(state.advert_intake.word())),
        (
            "microphone",
            Value::from(state.microphone.preference().word()),
        ),
        ("mic", Value::from(state.microphone.code().word())),
        ("mic_dictation", Value::from(state.mic_dictation.word())),
        (
            "mic_start_failures",
            Value::from(state.microphone.start_failures()),
        ),
        ("mic_source", Value::from(state.mic_source())),
        ("mic_underruns", Value::from(counts.underruns)),
        ("mic_concealed", Value::from(counts.concealed)),
        ("mic_decode_errors", Value::from(counts.decode_errors)),
        ("mic_limited", Value::from(counts.limited)),
        ("mic_nonfinite", Value::from(counts.nonfinite)),
    ]);
    fields.extend(counter_fields(&state.counters));
    object(fields)
}

/// The `_total` keys, one per counter.
fn counter_fields(counters: &Counters) -> [(&'static str, Value); 13] {
    [
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
    ]
}

/// Keys in the map's byte order, whatever `serde_json` features are on.
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

/// No reading is not available, not charging, level 0, as the C++ `Battery` reset leaves it.
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

/// A side that is not the primary reads the secondary, as the C++ does.
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
