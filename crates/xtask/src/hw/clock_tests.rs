use std::io::ErrorKind;
use std::time::Duration;

use clap::Parser;

use super::clock::{Clock, PausedClock, SystemClock};
use super::lab::{
    ACTION_TIMEOUT, ADOPTION_WAIT, DISCONNECT_WAIT, RECONNECT_WAIT, STATUS_TIMEOUT, wait_until_on,
};
use super::system::{Waited, wait_for_entries_on};
use super::unit::wait_for_exit;
use super::{Hw, HwCommand, Waits};

#[test]
fn stub_windows_are_independent_of_the_production_defaults() {
    let waits = |options: &[&str]| {
        let Hw {
            command: HwCommand::Link(link),
        } = Hw::try_parse_from(
            ["hw", "link", "--binary", "x"]
                .into_iter()
                .chain(options.iter().copied()),
        )
        .unwrap();
        link.waits()
    };
    let defaults = waits(&[]);
    assert_eq!(defaults.idle, Duration::from_secs(600));
    assert_eq!(defaults.burst, Duration::from_secs(3));
    assert_eq!(defaults.connect, Duration::from_secs(20));

    let stub = waits(&[
        "--idle-window-ms",
        "250",
        "--burst-window-ms",
        "400",
        "--connect-wait-ms",
        "800",
    ]);
    assert_eq!(stub.idle, Duration::from_millis(250));
    assert_eq!(stub.burst, Duration::from_millis(400));
    assert_eq!(stub.connect, Duration::from_millis(800));

    let seconds = waits(&["--idle-window-seconds", "1"]);
    assert_eq!(seconds.idle, Duration::from_secs(1));
    assert_eq!(seconds.burst, defaults.burst);
    assert_eq!(waits(&["--idle-window-seconds", "0"]).idle, Duration::ZERO);

    for option in ["--idle-window-ms", "--burst-window-ms", "--connect-wait-ms"] {
        for invalid in ["0", "1000", ""] {
            assert!(Hw::try_parse_from(["hw", "link", "--binary", "x", option, invalid]).is_err());
        }
    }
}

#[test]
fn wait_until_on_expires_at_the_connect_and_disconnect_defaults() {
    for (timeout, seconds) in [(Waits::default().connect, 20), (DISCONNECT_WAIT, 5)] {
        let clock = PausedClock::default();
        let last = std::cell::Cell::new(Duration::ZERO);
        assert!(
            !wait_until_on(&clock, timeout, || {
                last.set(clock.elapsed());
                false
            })
            .unwrap()
        );
        assert_eq!(clock.elapsed(), Duration::from_secs(seconds));
        assert_eq!(last.get(), clock.elapsed());
    }
}

#[test]
fn wait_for_entries_on_expires_at_the_adoption_and_reconnect_defaults() {
    for (timeout, seconds) in [(ADOPTION_WAIT, 10), (RECONNECT_WAIT, 15)] {
        let clock = PausedClock::default();
        let waited = wait_for_entries_on(
            &clock,
            timeout,
            || Ok(Vec::new()),
            |entries| !entries.is_empty(),
        )
        .unwrap();
        assert!(matches!(waited, Waited::TimedOut));
        assert_eq!(clock.elapsed(), Duration::from_secs(seconds));
    }
}

#[test]
fn the_system_clock_runs_the_journal_wait_to_its_deadline() {
    let clock = SystemClock;
    let began = clock.now();
    let timeout = Duration::from_millis(25);
    let waited = wait_for_entries_on(&clock, timeout, || Ok(Vec::new()), |_| false).unwrap();
    assert!(matches!(waited, Waited::TimedOut));
    assert!(clock.now().saturating_duration_since(began) >= timeout);
}

#[test]
fn the_client_deadline_distinguishes_an_exit_from_a_hung_command() {
    for (timeout, seconds) in [(STATUS_TIMEOUT, 5), (ACTION_TIMEOUT, 30)] {
        let clock = PausedClock::default();
        assert_eq!(
            wait_for_exit(&clock, timeout, || Ok(None::<()>)).unwrap(),
            None
        );
        assert_eq!(clock.elapsed(), Duration::from_secs(seconds));
    }
    let clock = PausedClock::default();
    let mut polled = false;
    let exited = wait_for_exit(&clock, STATUS_TIMEOUT, || {
        Ok(std::mem::replace(&mut polled, true).then_some(0))
    });
    assert_eq!(exited.unwrap(), Some(0));
    assert_eq!(clock.elapsed(), Duration::from_millis(5));
    let failed = wait_for_exit(&clock, STATUS_TIMEOUT, || {
        Err::<Option<()>, _>(ErrorKind::BrokenPipe.into())
    });
    assert_eq!(failed.unwrap_err().kind(), ErrorKind::BrokenPipe);
}
