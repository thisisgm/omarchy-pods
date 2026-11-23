//! Support tests: the pause before a failed save is tried again, on a paused clock.

use std::fs;
use std::time::Duration;

use pods_core::prefs::Prefs;
use tokio::sync::watch;
use tokio::time::{Instant, sleep, sleep_until};

use super::{FILE_NAME, follow};
use crate::daemon::paused::on_paused_clock;

/// `RETRY_PAUSE`, written out so a drifted pause goes red.
const RETRY_PAUSE: Duration = Duration::from_millis(2000);

/// Inside the pause, when the directory standing at the file's path is taken away.
const REPAIRED_AFTER: Duration = Duration::from_millis(500);

/// The paused clock's step, so the save is seen on neither side of the pause.
const TICK: Duration = Duration::from_millis(1);

#[test]
fn a_failed_save_is_tried_again_the_retry_pause_after() {
    on_paused_clock(async {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join(FILE_NAME);
        // A directory at the file's path fails every save until it is gone.
        fs::create_dir(&settings).unwrap();
        let (choices, changes) = watch::channel(Prefs::default());
        let follower = tokio::spawn(follow(changes, settings.clone()));
        let tried = Instant::now();
        choices.send_replace(Prefs::default());
        sleep(REPAIRED_AFTER).await;
        fs::remove_dir(&settings).unwrap();
        sleep_until(tried + RETRY_PAUSE - TICK).await;
        assert!(!settings.exists(), "saved before the pause ended");
        sleep_until(tried + RETRY_PAUSE + TICK).await;
        assert!(settings.is_file(), "not saved once the pause ended");
        drop(choices);
        follower.await.unwrap();
    });
}
