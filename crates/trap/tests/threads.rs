//! End to end: the trap's own thread, counted by name in a binary where no other test runs a trap at the same time.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use std::process::{Child, Command, Stdio};
    use std::sync::{Mutex, PoisonError};
    use std::time::{Duration, Instant};

    use pods_trap::{STOP_WAIT, TrapError, spawn};

    /// Far longer than a thread takes to be named or to leave /proc, far shorter than the idle stand-in's 2 s.
    const SETTLE: Duration = Duration::from_millis(500);
    const STAND_IN: &str = env!("CARGO_BIN_EXE_trap-dial");
    /// The name the trap gives its thread.
    const TRAP_THREAD: &str = "trap";

    /// Both tests count trap threads, so a parallel test runner must not overlap them.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

    /// A started stand-in, killed and reaped on every path out of the test.
    struct Reaped(Child);

    impl Drop for Reaped {
        fn drop(&mut self) {
            // An error here means the child is already gone, which is what this guard wants.
            let _killed = self.0.kill();
            let _reaped = self.0.wait();
        }
    }

    // Sample input: `/proc/self/task/<tid>/comm` holds `trap\n`.
    fn trap_threads() -> usize {
        std::fs::read_dir("/proc/self/task")
            .unwrap()
            .filter(|task| {
                let comm = task.as_ref().unwrap().path().join("comm");
                // A thread that ended between the listing and this read is not a trap thread.
                let name = std::fs::read_to_string(comm).unwrap_or_default();
                name.trim_end() == TRAP_THREAD
            })
            .count()
    }

    /// Std names a thread from inside it, and a joined thread leaves /proc a moment later, so the count settles.
    fn settled_trap_threads(want: usize) -> usize {
        let deadline = Instant::now().checked_add(SETTLE).unwrap();
        loop {
            let count = trap_threads();
            if count == want || Instant::now() >= deadline {
                return count;
            }
            std::thread::yield_now();
        }
    }

    #[test]
    fn dropping_the_trap_ends_its_thread() {
        let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
        let mut idle = Command::new(STAND_IN);
        idle.arg("idle").stdout(Stdio::null());
        let (child, trap) = spawn(idle, Vec::new()).unwrap();
        let mut child = Reaped(child);
        assert_eq!(
            settled_trap_threads(1),
            1,
            "waited {SETTLE:?} for the one trap thread"
        );
        let dropping = Instant::now();
        drop(trap);
        assert!(
            dropping.elapsed() < STOP_WAIT,
            "the drop took {:?}",
            dropping.elapsed()
        );
        assert_eq!(
            settled_trap_threads(0),
            0,
            "waited {SETTLE:?} for the trap thread to end"
        );
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "the child ended with the trap"
        );
    }

    #[test]
    fn a_child_that_cannot_start_is_a_spawn_error() {
        let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
        match spawn(Command::new(format!("{STAND_IN}.missing")), Vec::new()) {
            Ok((child, _trap)) => {
                let _reaped = Reaped(child);
                panic!("a missing program started");
            }
            Err(error) => assert!(matches!(error, TrapError::Spawn(_)), "{error:?}"),
        }
        assert_eq!(
            settled_trap_threads(0),
            0,
            "waited {SETTLE:?} for no trap thread"
        );
    }
}
