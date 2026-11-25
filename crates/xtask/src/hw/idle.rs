//! C5, the idle check: the daemon's wakeups over a window with the link up and one with nothing connected.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::Interrupted;
use super::clock::{Clock, SystemClock};
use super::lab::{DISCONNECT_WAIT, Lab, RECONNECT_WAIT, STATUS_TIMEOUT, wait_until, wait_until_on};
use super::report::Check;
use super::system::Wakeups;
use super::unit;

/// The size of one read of `librepods watch` output.
const WATCH_CHUNK_BYTES: usize = 4096;
/// G3 with the link up: wakeups a minute.
const IDLE_LINK_UP_PER_MINUTE: u64 = 12;
/// G3 with nothing connected: wakeups a minute.
const IDLE_DISCONNECTED_PER_MINUTE: u64 = 2;
const MILLIS_PER_MINUTE: u128 = Duration::from_secs(60).as_millis();
/// The rate is printed to one decimal place.
const TENTHS_PER_ONE: u128 = 10;
/// How often C5 reads the wakeups; tokio keeps an idle blocking thread 10 s, so each is read at least once before it ends.
const WAKEUP_SAMPLE_EVERY: Duration = Duration::from_secs(1);

/// Counts into `lines` the newlines `source` gives until it ends, through one fixed buffer so a line with no end cannot grow memory.
fn count_lines(mut source: impl Read, lines: &AtomicUsize) {
    let mut chunk = [0_u8; WATCH_CHUNK_BYTES];
    while let Ok(read @ 1..) = source.read(&mut chunk) {
        #[expect(
            clippy::naive_bytecount,
            reason = "a count over one small buffer needs no extra crate"
        )]
        let newlines = chunk.get(..read).map_or(0, |bytes| {
            bytes.iter().filter(|&&byte| byte == b'\n').count()
        });
        lines.fetch_add(newlines, Ordering::Relaxed);
    }
}

/// A `librepods watch` running for an idle window, counting the lines it prints; it is killed when dropped.
struct WatchLines {
    child: Child,
    reader: Option<JoinHandle<()>>,
    lines: Arc<AtomicUsize>,
}

impl WatchLines {
    fn start(lab: &Lab<'_>) -> Result<Self, String> {
        let mut child = Command::new(lab.binary)
            .arg("watch")
            .env("XDG_RUNTIME_DIR", lab.run_dir())
            .env("XDG_STATE_HOME", lab.dir.join("state"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(|error| format!("librepods watch could not start: {error}"))?;
        let stdout = child.stdout.take().ok_or("librepods watch has no output")?;
        let lines = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&lines);
        let reader = thread::spawn(move || count_lines(stdout, &counted));
        Ok(Self {
            child,
            reader: Some(reader),
            lines,
        })
    }

    /// Waits until the watch has printed its first line, which is the daemon's answer to its subscribe; false on a timeout.
    fn wait_first_line(&self, timeout: Duration) -> Result<bool, Interrupted> {
        wait_until(timeout, || self.lines.load(Ordering::Relaxed) > 0)
    }

    /// Ends the watch and returns the lines it printed after its first; a watch that had already ended is an error.
    fn finish(mut self) -> Result<usize, String> {
        let ended = self.child.try_wait();
        let lines = self.stop().saturating_sub(1);
        match ended {
            Ok(None) => Ok(lines),
            Ok(Some(status)) => Err(format!(
                "librepods watch ended before the window did: {status}"
            )),
            Err(error) => Err(format!("librepods watch could not be checked: {error}")),
        }
    }

    fn stop(&mut self) -> usize {
        let killed = self.child.kill();
        let reaped = self.child.wait();
        drop((killed, reaped));
        if let Some(reader) = self.reader.take() {
            drop(reader.join());
        }
        self.lines.load(Ordering::Relaxed)
    }
}

impl Drop for WatchLines {
    fn drop(&mut self) {
        if self.reader.is_some() {
            self.stop();
        }
    }
}

/// C5: wakeups of the daemon over a window with the link up, then one with nothing connected; G3 bounds both.
pub(crate) fn idle(lab: &Lab<'_>) -> Result<Check, Interrupted> {
    let mut check = Check::new("C5 idle");
    let pid = match unit::main_pid() {
        Ok(pid) => pid,
        Err(error) => {
            check.fail(error);
            return Ok(check);
        }
    };
    if lab.connected() == Some(true) {
        idle_window(lab, pid, "link up", IDLE_LINK_UP_PER_MINUTE, &mut check)?;
    } else {
        check.fail("the link was not up for the link up window");
    }
    match lab.act("disconnect") {
        Ok(()) if wait_until(DISCONNECT_WAIT, || lab.connected() == Some(false))? => {
            idle_window(
                lab,
                pid,
                "nothing connected",
                IDLE_DISCONNECTED_PER_MINUTE,
                &mut check,
            )?;
        }
        Ok(()) => check.fail("still connected after do disconnect"),
        Err(error) => check.fail(error),
    }
    // The run leaves the AirPods connected for the installed service to adopt.
    match lab.act("connect") {
        Ok(()) if wait_until(RECONNECT_WAIT, || lab.connected() == Some(true))? => {
            check.note("connected again after the windows");
        }
        Ok(()) => check.fail(format!(
            "not connected again after the windows: still down {} s after do connect",
            RECONNECT_WAIT.as_secs()
        )),
        Err(error) => check.fail(format!("not connected again after the windows: {error}")),
    }
    Ok(check)
}

/// Waits out `window`, reading the wakeups every `WAKEUP_SAMPLE_EVERY` and once at its end; a failed read keeps its error.
fn sample_through(
    window: Duration,
    wakeups: &mut Result<Wakeups, String>,
) -> Result<(), Interrupted> {
    sample_through_on(&SystemClock, window, || {
        if let Ok(counter) = wakeups.as_mut()
            && let Err(error) = counter.sample()
        {
            *wakeups = Err(error);
        }
    })
}

fn sample_through_on(
    clock: &dyn Clock,
    window: Duration,
    mut sample: impl FnMut(),
) -> Result<(), Interrupted> {
    let end = clock.deadline(window);
    loop {
        let left = clock.remaining(end);
        wait_until_on(clock, left.min(WAKEUP_SAMPLE_EVERY), || false)?;
        sample();
        if left <= WAKEUP_SAMPLE_EVERY {
            return Ok(());
        }
    }
}

fn idle_window(
    lab: &Lab<'_>,
    pid: u32,
    name: &str,
    bound_per_minute: u64,
    check: &mut Check,
) -> Result<(), Interrupted> {
    let discovering_before = lab.adapter_state().map(|(_, discovering)| discovering);
    let watch = match WatchLines::start(lab) {
        Ok(watch) => watch,
        Err(error) => {
            check.fail(format!("{name}: {error}"));
            return Ok(());
        }
    };
    // The daemon's accept and first write for the watch are not idle wakeups, so counting starts after them.
    if !watch.wait_first_line(STATUS_TIMEOUT)? {
        check.fail(format!(
            "{name}: librepods watch printed nothing within {} s",
            STATUS_TIMEOUT.as_secs()
        ));
        return Ok(());
    }
    let mut wakeups = Wakeups::start(pid);
    sample_through(lab.waits.idle, &mut wakeups)?;
    let lines = match watch.finish() {
        Ok(lines) => lines,
        Err(error) => {
            check.fail(format!("{name}: {error}"));
            return Ok(());
        }
    };
    let discovering_after = lab.adapter_state().map(|(_, discovering)| discovering);
    match wakeups {
        Ok(wakeups) => report_wakeups(
            check,
            name,
            wakeups.woke(),
            lab.waits.idle,
            bound_per_minute,
            lines,
        ),
        Err(error) => check.fail(error),
    }
    for (end, discovering) in [("start", discovering_before), ("end", discovering_after)] {
        match discovering {
            Ok(false) => {}
            Ok(true) => check.fail(format!("{name}: the adapter was discovering at the {end}")),
            Err(error) => check.fail(error),
        }
    }
    Ok(())
}

fn report_wakeups(
    check: &mut Check,
    name: &str,
    woke: u64,
    window: Duration,
    bound_per_minute: u64,
    lines: usize,
) {
    let window_ms = window.as_millis();
    let rate_numerator = u128::from(woke).saturating_mul(MILLIS_PER_MINUTE);
    let rate = rate_numerator
        .saturating_mul(TENTHS_PER_ONE)
        .checked_div(window_ms)
        .map_or_else(
            || "rate unavailable".to_owned(),
            |tenths| {
                format!(
                    "{}.{} a minute",
                    tenths.checked_div(TENTHS_PER_ONE).unwrap_or(0),
                    tenths.checked_rem(TENTHS_PER_ONE).unwrap_or(0)
                )
            },
        );
    check.note(format!(
        "G3 {name}: {woke} wakeups in {window_ms} ms, {rate}, bound at most {bound_per_minute}, watch lines {lines}"
    ));
    if rate_numerator > u128::from(bound_per_minute).saturating_mul(window_ms) {
        check.fail(format!(
            "G3 {name}: over {bound_per_minute} wakeups a minute"
        ));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;
    use std::{process, thread};

    use super::super::system::current_thread_id;
    use super::super::unit::pause;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use proptest::prelude::*;

    use super::{
        Check, WAKEUP_SAMPLE_EVERY, WATCH_CHUNK_BYTES, Wakeups, count_lines, report_wakeups,
        sample_through,
    };

    /// How many times the thread under count sleeps, so wakes, before it idles.
    const SLEEPS: u64 = 200;
    /// Long enough that the thread under count ends inside it.
    const WINDOW: Duration = Duration::from_secs(4);

    #[test]
    fn a_subsecond_idle_window_can_miss_g3() {
        const WINDOW: Duration = Duration::from_millis(250);
        const BOUND_PER_MINUTE: u64 = 120;
        let mut check = Check::new("C5 idle");
        report_wakeups(&mut check, "link up", 1, WINDOW, BOUND_PER_MINUTE, 0);
        assert!(check.failed(), "{:?}", check.lines);
        assert_eq!(
            check.lines,
            [
                "G3 link up: 1 wakeups in 250 ms, 240.0 a minute, bound at most 120, watch lines 0",
                "G3 link up: over 120 wakeups a minute",
            ]
        );
    }

    #[test]
    pub(super) fn the_window_is_read_often_enough_to_count_a_thread_that_ends() {
        let mut wakeups = Wakeups::start(process::id());
        let (named, sleeper_id) = mpsc::channel();
        let sleeper = thread::spawn(move || {
            named.send(current_thread_id()).unwrap();
            for _ in 0..SLEEPS {
                pause(Duration::from_millis(1));
            }
            pause(
                WAKEUP_SAMPLE_EVERY
                    .saturating_mul(3)
                    .checked_div(2)
                    .unwrap(),
            );
        });
        let sleeper_id = sleeper_id.recv().unwrap();
        sample_through(WINDOW, &mut wakeups).unwrap();
        assert!(sleeper.is_finished(), "the thread outlived the window");
        sleeper.join().unwrap();
        let woke = wakeups.unwrap().woke_by(sleeper_id);
        assert!(woke >= SLEEPS, "{woke} wakeups");
    }

    proptest! {
        #[test]
        fn any_watch_bytes_count_exactly_their_newlines(
            bytes in proptest::collection::vec(any::<u8>(), 0..=WATCH_CHUNK_BYTES * 3)
        ) {
            let lines = AtomicUsize::new(0);
            count_lines(bytes.as_slice(), &lines);
            let newlines = bytes.split(|&byte| byte == b'\n').count().saturating_sub(1);
            prop_assert_eq!(lines.load(Ordering::Relaxed), newlines);
        }
    }
}

#[cfg(test)]
mod clock_tests {
    use std::time::Duration;

    use super::super::Waits;
    use super::super::clock::PausedClock;
    use super::sample_through_on;

    #[test]
    fn each_idle_window_samples_every_second_and_at_the_end() {
        let clock = PausedClock::default();
        let mut samples = Vec::new();
        sample_through_on(&clock, Waits::default().idle, || {
            samples.push(clock.elapsed());
        })
        .unwrap();
        assert!(samples.into_iter().eq((1..=600).map(Duration::from_secs)));
        assert_eq!(clock.elapsed(), Duration::from_secs(600));

        let clock = PausedClock::default();
        let window = Duration::from_millis(1250);
        let mut samples = Vec::new();
        sample_through_on(&clock, window, || samples.push(clock.elapsed())).unwrap();
        assert_eq!(
            samples,
            [Duration::from_secs(1), Duration::from_millis(1300)]
        );
    }
}
