//! C5, the idle check: the daemon's wakeups over a window with the link up and one with nothing connected.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::Interrupted;
use super::lab::{DISCONNECT_WAIT, Lab, RECONNECT_WAIT, STATUS_TIMEOUT, wait_out, wait_until};
use super::report::Check;
use super::system::Wakeups;
use super::unit;

/// The size of one read of `librepods watch` output.
const WATCH_CHUNK_BYTES: usize = 4096;
/// G3 with the link up: wakeups a minute.
const IDLE_LINK_UP_PER_MINUTE: u64 = 12;
/// G3 with nothing connected: wakeups a minute.
const IDLE_DISCONNECTED_PER_MINUTE: u64 = 2;
const SECONDS_PER_MINUTE: u64 = 60;
/// The rate is printed to one decimal place.
const TENTHS_PER_ONE: u64 = 10;
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
    fn wait_first_line(&self) -> Result<bool, Interrupted> {
        wait_until(STATUS_TIMEOUT, || self.lines.load(Ordering::Relaxed) > 0)
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
    let end = Instant::now().checked_add(window);
    loop {
        let left = end.map_or(Duration::ZERO, |end| {
            end.saturating_duration_since(Instant::now())
        });
        wait_out(left.min(WAKEUP_SAMPLE_EVERY))?;
        if let Ok(counter) = wakeups.as_mut()
            && let Err(error) = counter.sample()
        {
            *wakeups = Err(error);
        }
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
    if !watch.wait_first_line()? {
        check.fail(format!(
            "{name}: librepods watch printed nothing within {} s",
            STATUS_TIMEOUT.as_secs()
        ));
        return Ok(());
    }
    let mut wakeups = Wakeups::start(pid);
    sample_through(lab.idle_window, &mut wakeups)?;
    let lines = match watch.finish() {
        Ok(lines) => lines,
        Err(error) => {
            check.fail(format!("{name}: {error}"));
            return Ok(());
        }
    };
    let discovering_after = lab.adapter_state().map(|(_, discovering)| discovering);
    let seconds = lab.idle_window.as_secs().max(1);
    match wakeups {
        Ok(wakeups) => {
            let woke = wakeups.woke();
            let tenths = woke
                .saturating_mul(SECONDS_PER_MINUTE)
                .saturating_mul(TENTHS_PER_ONE)
                .checked_div(seconds)
                .unwrap_or(0);
            check.note(format!(
                "G3 {name}: {woke} wakeups in {seconds} s, {}.{} a minute, bound at most {bound_per_minute}, watch lines {lines}",
                tenths.checked_div(TENTHS_PER_ONE).unwrap_or(0),
                tenths.checked_rem(TENTHS_PER_ONE).unwrap_or(0)
            ));
            if woke.saturating_mul(SECONDS_PER_MINUTE) > bound_per_minute.saturating_mul(seconds) {
                check.fail(format!(
                    "G3 {name}: over {bound_per_minute} wakeups a minute"
                ));
            }
        }
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

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;
    use std::{process, thread};

    use super::super::system::current_thread_id;
    use super::super::unit::pause;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use proptest::prelude::*;

    use super::{WAKEUP_SAMPLE_EVERY, WATCH_CHUNK_BYTES, Wakeups, count_lines, sample_through};

    /// How many times the thread under count sleeps, so wakes, before it idles.
    const SLEEPS: u64 = 200;
    /// Long enough that the thread under count ends inside it.
    const WINDOW: Duration = Duration::from_secs(4);

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
