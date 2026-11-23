//! The daemon's settings through the built client, and the `settings.json` a test plants before a start.

use std::fs::{self, DirBuilder};
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;

use serde_json::Value;

use super::{DIR_MODE, Daemon, Session, binary, wait_for};

/// How the daemon's line about settings it cannot take starts, from `src/daemon/prefs.rs`.
pub(crate) const UNREADABLE: &str = "prefs: settings unreadable, defaults used";

/// Writes `bytes` where the daemon keeps its settings, its directory made as the daemon would.
pub(crate) fn plant_settings(session: &Session, bytes: &[u8]) {
    let path = session.settings_file();
    DirBuilder::new()
        .recursive(true)
        .mode(DIR_MODE)
        .create(path.parent().unwrap())
        .unwrap();
    fs::write(path, bytes).unwrap();
}

pub(crate) fn set(session: &Session, key: &str, value: &str) {
    let output = session.run_client(Path::new(binary()), &["set", key, value]);
    assert!(output.status.success(), "set {key} {value}: {output:?}");
}

/// The legacy `status` verb's document.
pub(crate) fn status(session: &Session) -> Value {
    serde_json::from_str(&session.status()).unwrap()
}

/// The settings file once its `key` holds `word`, parsed.
pub(crate) fn wait_settings(session: &Session, key: &str, word: &str) -> Value {
    wait_for(&format!("settings.json with {key} {word}"), || {
        let text = fs::read_to_string(session.settings_file()).ok()?;
        let settings: Value = serde_json::from_str(&text).ok()?;
        (settings[key] == word).then_some(settings)
    })
}

pub(crate) fn unreadable_lines(daemon: &Daemon) -> usize {
    daemon
        .log()
        .lines()
        .filter(|line| line.starts_with(UNREADABLE))
        .count()
}

pub(crate) fn stop(daemon: &mut Daemon) {
    let status = daemon.stop();
    assert!(
        status.success(),
        "the daemon exited with {status}: {}",
        daemon.log()
    );
}
