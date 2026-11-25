//! The microphone uplink as the tests against the built daemon drive it: the settings a start keeps, a read barrier on the pods' packets, and the STARTs and STOPs the pods heard.

use std::fs;
use std::rc::Rc;
use std::time::{Duration, Instant};

use pods_core::policy::audio::RESTORE_HOLD_MS;
use pods_core::policy::mic::{START_ANSWER_MS, START_TRIES, STOP_ANSWER_MS};
use pods_core::prefs::{self, MicPref, Prefs};
use pods_fake::{Heard, Profile};
use pods_samples::uplink::{START, STOP};
use pods_samples::{CASE, Cell, LEFT, RIGHT};
use pulseaudio::protocol::SinkState;
use serde_json::{Value, json};

use super::audio::wire::sink_named;
use super::audio::{Asked, FakeAudio, PERCENT, writes_of};
use super::rig::{Device, REAL_TIME_SLACK, Rig, assert_gap, connected};
use super::settings::plant_settings;
use super::{HANG_LIMIT, Session, wait_for};

/// How late the resume may come after the hold; both instants are the fake server's, so only the daemon's timer is in it.
const HOLD_SLACK: Duration = Duration::from_millis(100);

/// How long a START waits for the running marker before the next try.
pub(crate) const START_ANSWER: Duration = Duration::from_millis(START_ANSWER_MS);
/// How long a STOP waits for the stopped marker before the restore goes ahead.
pub(crate) const STOP_ANSWER: Duration = Duration::from_millis(STOP_ANSWER_MS);
/// The least gap two STARTs' receive stamps may show, as each carries the fake's own read latency.
pub(crate) const TRY_SPACING: Duration = START_ANSWER.saturating_sub(REAL_TIME_SLACK);
const _: () = assert!(
    !TRY_SPACING.is_zero(),
    "the slack would leave no spacing to check"
);

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

/// As `rig_with`, with the fake audio server holding the pods' sink as its default in `state`.
pub(crate) fn rig_with_audio(
    preference: &str,
    models: &[&str],
    state: SinkState,
) -> (Rig, Device, Rc<FakeAudio>) {
    let mut rig = Rig::new();
    keep_settings(rig.session(), preference, models);
    let audio = rig.start_audio();
    let device = rig.add_pods(Profile::default());
    let sink = sink_named(device.address);
    audio.add_sink(&sink, PERCENT);
    audio.set_default_sink(&sink);
    audio.set_sink_state(&sink, state);
    rig.start();
    (rig, device, audio)
}

/// As `link`, then the pods' sink read and applied.
pub(crate) fn link_heard(rig: &Rig, device: &Device) {
    link(rig, device);
    rig.wait_sink_told();
}

/// Moves the pods' sink to `state`, then waits for the daemon to apply what it read.
pub(crate) fn move_sink(rig: &Rig, audio: &FakeAudio, device: &Device, state: SinkState) {
    audio.set_sink_state(&sink_named(device.address), state);
    rig.wait_sink_told();
}

/// Fails unless the resume came the whole restore hold after the suspend, and not `HOLD_SLACK` later.
pub(crate) fn assert_hold(suspended: Instant, resumed: Instant) {
    let hold = Duration::from_millis(RESTORE_HOLD_MS);
    let range = hold..=hold.saturating_add(HOLD_SLACK);
    assert_gap("the hold", suspended, resumed, &range);
}

/// The pods' sink suspended then resumed, and nothing more written before the daemon read again or left: when each was asked.
pub(crate) fn wait_restore(audio: &FakeAudio, device: &Device) -> (Instant, Instant) {
    let sink = sink_named(device.address);
    let writes = wait_for("two writes, then a read or the daemon gone", || {
        // A client leaves only after its last request is recorded, so `gone` is read first.
        let gone = audio.clients() == 0;
        let requests = audio.requests();
        let settled = gone || requests.last().is_some_and(Asked::only_reads);
        let writes = writes_of(&requests);
        (writes.len() >= 2 && settled).then_some(writes)
    });
    match writes.as_slice() {
        [
            Asked::Suspend {
                name: Some(first),
                suspend: true,
                at: suspended,
            },
            Asked::Suspend {
                name: Some(second),
                suspend: false,
                at: resumed,
            },
        ] if *first == sink && *second == sink => (*suspended, *resumed),
        writes => panic!("a suspend and a resume of {sink}, and nothing else: {writes:?}"),
    }
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

/// The STARTs one want sends before it gives up.
pub(crate) fn all_tries() -> usize {
    usize::try_from(START_TRIES).unwrap()
}

/// The STARTs the status counts as gone unanswered.
pub(crate) fn start_failures(rig: &Rig) -> u64 {
    rig.status()["mic_start_failures"].as_u64().unwrap()
}

pub(crate) fn mic(rig: &Rig) -> String {
    rig.status()["mic"].as_str().unwrap().to_owned()
}

pub(crate) fn wait_mic(rig: &Rig, word: &str) -> Value {
    rig.wait_status(&format!("mic {word}"), |status| status["mic"] == word)
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
