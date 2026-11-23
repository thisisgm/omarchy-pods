//! The microphone uplink as the tests against the built daemon drive it: the settings a start keeps, a read barrier on the pods' packets, and the STARTs and STOPs the pods heard.

use std::fs;

use pods_core::prefs::{self, MicPref, Prefs};
use pods_fake::{Heard, Profile};
use pods_samples::uplink::{START, STOP};
use pods_samples::{CASE, Cell, LEFT, RIGHT};
use serde_json::{Value, json};

use super::rig::{Device, Rig, connected};
use super::settings::plant_settings;
use super::{HANG_LIMIT, Session, wait_for};

/// Two left levels no fixture starts with; a barrier moves to whichever the status does not show.
const BARRIER_LEVELS: [u8; 2] = [42, 43];

/// Plants `settings.json` with `preference` and the confirmed `models`, the other choices at their defaults.
pub(crate) fn keep_settings(session: &Session, preference: &str, models: &[&str]) {
    let mut kept = Prefs {
        microphone: MicPref::from_word(preference).unwrap(),
        ..Prefs::default()
    };
    for model in models {
        kept.mic_models.confirm(model);
    }
    plant_settings(session, prefs::render(&kept).as_bytes());
}

/// Keeps the microphone off, so a test of something else links with no uplink and a `mic` that never moves.
pub(crate) fn keep_off(session: &Session) {
    keep_settings(session, "off", &[]);
}

/// The daemon started with `preference` and the confirmed `models` kept, and pods placed as `profile` says, not linked yet.
pub(crate) fn rig_with(preference: &str, models: &[&str], profile: Profile) -> (Rig, Device) {
    let mut rig = Rig::new();
    keep_settings(rig.session(), preference, models);
    let device = rig.add_pods(profile);
    rig.start();
    (rig, device)
}

/// BlueZ reports the pods connected, then every packet of their first burst is read.
pub(crate) fn link(rig: &Rig, device: &Device) {
    rig.link_up(device);
    rig.wait_filled();
    read_up_to_now(rig, device);
}

/// BlueZ reports the pods gone, then the status shows the link down.
pub(crate) fn lose(rig: &Rig, device: &Device) {
    rig.link_down(device);
    rig.wait_status("the link down", |status| !connected(status));
}

/// Returns once the daemon read every packet the pods sent before the call: a battery level sent after them shows in the status.
pub(crate) fn read_up_to_now(rig: &Rig, device: &Device) {
    let shown = rig.status()["left"]["level"].clone();
    let level = BARRIER_LEVELS
        .into_iter()
        .find(|level| shown != *level)
        .unwrap();
    let left = Cell { level, ..LEFT };
    device.pods.set_battery(left, RIGHT, CASE);
    rig.wait_status("the barrier's battery level", |status| {
        status["left"]["level"] == level
    });
}

pub(crate) fn mic(rig: &Rig) -> String {
    rig.status()["mic"].as_str().unwrap().to_owned()
}

pub(crate) fn wait_mic(rig: &Rig, word: &str) {
    rig.wait_status(&format!("mic {word}"), |status| status["mic"] == word);
}

pub(crate) fn starts(device: &Device) -> Vec<Heard> {
    heard(device, &START)
}

pub(crate) fn stops(device: &Device) -> Vec<Heard> {
    heard(device, &STOP)
}

/// Every START heard, once there are `count`.
pub(crate) fn wait_starts(device: &Device, count: usize) -> Vec<Heard> {
    device
        .pods
        .wait_heard(&START, count, HANG_LIMIT)
        .unwrap_or_else(|deadline| panic!("{count} STARTs: {deadline:?}"))
}

/// The first STOP heard, once there is one.
pub(crate) fn wait_stop(device: &Device) -> Heard {
    let stops = device
        .pods
        .wait_heard(&STOP, 1, HANG_LIMIT)
        .unwrap_or_else(|deadline| panic!("a STOP: {deadline:?}"));
    stops.into_iter().next().unwrap()
}

/// The confirmed model numbers `settings.json` holds now.
pub(crate) fn kept_models(session: &Session) -> Value {
    let text = fs::read_to_string(session.settings_file()).unwrap();
    serde_json::from_str::<Value>(&text).unwrap()["mic_models"].clone()
}

/// Waits for `settings.json` to hold exactly `models`.
pub(crate) fn wait_kept_models(session: &Session, models: &[&str]) {
    let wanted = json!(models);
    wait_for(&format!("settings.json with mic_models {wanted}"), || {
        (kept_models(session) == wanted).then_some(())
    });
}

fn heard(device: &Device, sdu: &[u8]) -> Vec<Heard> {
    let all = device.pods.heard();
    all.into_iter().filter(|heard| heard.sdu == sdu).collect()
}
