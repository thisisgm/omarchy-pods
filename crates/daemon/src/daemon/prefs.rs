//! The choices kept across restarts in `settings.json` beside the state file, and the one read of the C++ daemon's file.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use pods_core::prefs::{self, Prefs};
use tokio::sync::watch;
use tokio::task::spawn_blocking;
use tokio::time::timeout;

use super::owner_file::{Failures, WriteError};
use super::{legacy_conf::LegacyConf, log, owner_file};

pub(super) const FILE_NAME: &str = "settings.json";

const UNREADABLE: &str = "prefs: settings unreadable, defaults used";

/// A repair (space freed, a path fixed) is a person's seconds, and one small write per pause costs nothing.
const RETRY_PAUSE: Duration = Duration::from_secs(2);

/// A save that failed, worded as the journal shows it.
#[derive(Debug, PartialEq)]
struct Unsaved {
    path: PathBuf,
    step: WriteError,
}

impl fmt::Display for Unsaved {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "prefs: cannot write {}: {}",
            self.path.display(),
            self.step
        )
    }
}

/// Loads settings at startup, falling back to the legacy preferences when `settings` is absent.
pub(super) fn load(settings: &Path, legacy: &LegacyConf<'_>) -> Prefs {
    match owner_file::read(settings) {
        Ok(Some(bytes)) => match prefs::parse(&String::from_utf8_lossy(&bytes)) {
            Ok(prefs) => prefs,
            Err(error) => unreadable(&error.to_string()),
        },
        Ok(None) => legacy
            .read()
            .map_or_else(Prefs::default, |old| migrate(settings, old)),
        Err(error) => unreadable(&error.to_string()),
    }
}

fn unreadable(reason: &str) -> Prefs {
    log(&format!("{UNREADABLE}: {reason}"));
    Prefs::default()
}

/// Saves both migrated preferences together for the next start.
fn migrate(settings: &Path, old: &[u8]) -> Prefs {
    let taken = prefs::from_old_conf(&String::from_utf8_lossy(old));
    // A failed save needs no retry here: the next start reads the settings file if the rename landed, else the old file again.
    if let Err(unsaved) = store(settings, &taken) {
        log(&unsaved.to_string());
    }
    taken
}

fn store(settings: &Path, prefs: &Prefs) -> Result<(), Unsaved> {
    owner_file::write(settings, prefs::render(prefs).as_bytes()).map_err(|step| Unsaved {
        path: settings.to_path_buf(),
        step,
    })
}

/// Writes the latest value, a failed one again every `RETRY_PAUSE`; once the driver drops the sender it makes one last try and returns.
pub(super) async fn follow(mut changes: watch::Receiver<Prefs>, settings: PathBuf) {
    let mut failures = Failures::new(format!("prefs: {} is written again", settings.display()));
    loop {
        let open = if failures.failing() {
            // The owed value waits out the pause, unless a newer value or the stop comes first.
            timeout(RETRY_PAUSE, changes.changed())
                .await
                .unwrap_or(Ok(()))
                .is_ok()
        } else {
            changes.changed().await.is_ok()
        };
        if !open && !failures.failing() {
            return;
        }
        let prefs = changes.borrow_and_update().clone();
        let path = settings.clone();
        match spawn_blocking(move || store(&path, &prefs)).await {
            Ok(result) => failures.note(result),
            Err(error) => log(&format!("prefs: the write did not finish: {error}")),
        }
        if !open {
            return;
        }
    }
}

#[cfg(test)]
mod tests;
