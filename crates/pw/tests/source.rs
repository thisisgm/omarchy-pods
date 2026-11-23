//! Support tests: the binding against the real `pipewire` program of this machine through the rig, its only proof; no user path yet.

mod support;

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;
    use std::fs;
    use std::io::ErrorKind;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
    use std::thread;
    use std::time::{Duration, Instant};

    use pods_pw::source::PW_OPEN_MS;
    use pods_pw::{Event, Fill, OpenError, Source, Spec};
    use pods_pwrig::tone::{CHECKED, SETTLE, assert_tone, frames};
    use pods_pwrig::{DRIVER_NODE, Recorder, Server, node_names, states_latency};
    use serde_json::json;

    use crate::support::{DESCRIPTION, LATENCY_NS, NAME, PRIORITY, RATE, isolate, spec};

    /// Half of full scale.
    const AMPLITUDE: f64 = 0.5;
    const LEVEL_TOLERANCE_DB: f64 = 0.5;
    /// Recorded beyond the settle and the checked span, for the silence before the first sound.
    const LEAD: Duration = Duration::from_millis(200);
    /// A local server answers in milliseconds; this is a hung one.
    const WITHIN: Duration = Duration::from_secs(5);
    /// How long `open` may take past its bound to end what it started, on a loaded host.
    const OPEN_SLACK: Duration = Duration::from_secs(1);
    /// The keys a device or a driver would carry, and the three that would link the node or name a server.
    const FORBIDDEN: [&str; 10] = [
        "device.id",
        "device.api",
        "card.profile.device",
        "device.routes",
        "bluez5.loopback",
        "priority.driver",
        "node.driver",
        "node.autoconnect",
        "target.object",
        "remote.name",
    ];
    /// Graph cycles each half of the ratio test counts, about 5 s at the rig's cycle.
    const COUNTED_CYCLES: u64 = 120;
    /// Cycles left out before each half, for the converter to settle on its ratio.
    const SETTLE_CYCLES: u64 = 12;
    const RATIO: f64 = 1.001;
    /// In the kernel's name for the library's data thread: `pw-data-loop` on 1.0.5, `data-loop.0` on 1.4.2.
    const DATA_THREAD: &str = "data-loop";
    /// The kernel's name for the loop thread, which the binding names, with its line end.
    const LOOP_THREAD: &str = "pods-pw\n";
    /// How far the rise in frames may stand from the ratio's: a quarter of it.
    const RATIO_TOLERANCE: f64 = (RATIO - 1.0) / 4.0;

    /// A sine at `AMPLITUDE`, continuous across buffers.
    struct Tone {
        hz: f64,
        frame: u32,
    }

    impl Fill for Tone {
        #[expect(
            clippy::as_conversions,
            clippy::cast_possible_truncation,
            reason = "a value inside full scale, as the float the node sends"
        )]
        fn fill(&mut self, out: &mut [f32]) -> f64 {
            for sample in out {
                let seconds = f64::from(self.frame) / f64::from(RATE);
                *sample = ((TAU * self.hz * seconds).sin() * AMPLITUDE) as f32;
                self.frame = self.frame.wrapping_add(1);
            }
            1.0
        }
    }

    /// Counts the frames asked for over two runs of cycles, the second at `RATIO`, then says so once.
    struct Counted {
        cycle: u64,
        frames: [Arc<AtomicU64>; 2],
        done: SyncSender<()>,
    }

    impl Fill for Counted {
        fn fill(&mut self, out: &mut [f32]) -> f64 {
            out.fill(0.0);
            let half = SETTLE_CYCLES + COUNTED_CYCLES;
            let asked = u64::try_from(out.len()).unwrap();
            if (SETTLE_CYCLES..half).contains(&self.cycle) {
                self.frames[0].fetch_add(asked, Ordering::Relaxed);
            } else if (half + SETTLE_CYCLES..2 * half).contains(&self.cycle) {
                self.frames[1].fetch_add(asked, Ordering::Relaxed);
            } else if self.cycle == 2 * half {
                self.done.try_send(()).unwrap_or(());
            }
            self.cycle = self.cycle.saturating_add(1);
            if self.cycle > half { RATIO } else { 1.0 }
        }
    }

    /// Says once which thread runs it, by the name the kernel has for it.
    struct Named(SyncSender<String>);

    impl Fill for Named {
        fn fill(&mut self, out: &mut [f32]) -> f64 {
            out.fill(0.0);
            self.0.try_send(thread_name()).unwrap_or(());
            1.0
        }
    }

    /// The kernel's name for the thread that calls it, as in `data-loop.0\n`; empty when it cannot be read, for a callback never panics.
    fn thread_name() -> String {
        fs::read_to_string("/proc/thread-self/comm").unwrap_or_default()
    }

    fn open(
        socket: &Path,
        fill: impl Fill + 'static,
    ) -> Result<(Source, Receiver<Event>), OpenError> {
        // Every socket a test opens is in its own directory.
        isolate(socket.parent().unwrap());
        let (events, seen) = mpsc::channel();
        let report = Box::new(move |event| events.send(event).unwrap_or(()));
        Ok((Source::open(&spec(socket), Box::new(fill), report)?, seen))
    }

    fn tone(hz: f64) -> Tone {
        Tone { hz, frame: 0 }
    }

    fn threads() -> usize {
        fs::read_dir("/proc/self/task").unwrap().count()
    }

    #[test]
    fn a_source_appears_with_exactly_its_properties() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let (_source, _events) = open(&server.socket(), tone(1_000.0)).unwrap();
        // No wait: `open` returned, so the node is there.
        let node = server.wait_node(NAME, Duration::ZERO);
        let props = &node["info"]["props"];
        let expected = json!({
            "media.class": "Audio/Source",
            "node.name": NAME,
            "node.description": DESCRIPTION,
            "node.virtual": false,
            "priority.session": PRIORITY,
            "node.slow-resume": true,
        });
        for (key, value) in expected.as_object().unwrap() {
            assert_eq!(props.get(key), Some(value), "{key} in {props}");
        }
        for key in FORBIDDEN {
            assert_eq!(props.get(key), None, "{key} in {props}");
        }
    }

    #[test]
    fn a_linked_recorder_records_what_fill_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let (_source, _events) = open(&server.socket(), tone(1_000.0)).unwrap();
        let recorder = Recorder::superwhisper(&server, NAME, &dir.path().join("capture.wav"));
        server.link(NAME, recorder.node());
        let rate = recorder.rate();
        recorder.wait_frames(frames(LEAD + SETTLE + CHECKED, rate), WITHIN);
        let samples = recorder.finish();
        assert_tone(&samples, rate, 1_000.0, AMPLITUDE, LEVEL_TOLERANCE_DB);
    }

    #[test]
    fn a_10_khz_tone_reaches_a_48_khz_recorder() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let (_source, _events) = open(&server.socket(), tone(10_000.0)).unwrap();
        let recorder = Recorder::history(&server, NAME, &dir.path().join("history.wav"));
        server.link(NAME, recorder.node());
        let rate = recorder.rate();
        recorder.wait_frames(frames(LEAD + SETTLE + CHECKED, rate), WITHIN);
        let samples = recorder.finish();
        assert_tone(&samples, rate, 10_000.0, AMPLITUDE, LEVEL_TOLERANCE_DB);
    }

    #[test]
    fn fill_runs_on_the_data_thread_and_events_on_the_loop() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        isolate(dir.path());
        let socket = server.socket();
        let (filled, fill_thread) = mpsc::sync_channel(1);
        let (changed, event_thread) = mpsc::sync_channel(1);
        let events = Box::new(move |_| changed.try_send(thread_name()).unwrap_or(()));
        let _source = Source::open(&spec(&socket), Box::new(Named(filled)), events).unwrap();
        let recorder = Recorder::superwhisper(&server, NAME, &dir.path().join("capture.wav"));
        server.link(NAME, recorder.node());
        let fill_thread = fill_thread.recv_timeout(WITHIN).unwrap();
        assert!(
            fill_thread.contains(DATA_THREAD),
            "fill ran on {fill_thread}"
        );
        assert_eq!(event_thread.recv_timeout(WITHIN).unwrap(), LOOP_THREAD);
    }

    #[test]
    fn a_test_that_opens_a_source_names_no_message_bus() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let (_source, _events) = open(&server.socket(), tone(1_000.0)).unwrap();
        for name in ["DBUS_SESSION_BUS_ADDRESS", "DBUS_SYSTEM_BUS_ADDRESS"] {
            let address = std::env::var(name).unwrap();
            let socket = Path::new(address.strip_prefix("unix:path=").unwrap());
            assert!(socket.starts_with(dir.path()), "{name} is {address}");
            assert!(!socket.exists(), "{address} exists");
        }
    }

    #[test]
    fn a_rate_ratio_is_followed_by_the_server() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let frames = [Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0))];
        let (done, finished) = mpsc::sync_channel(1);
        let counted = Counted {
            cycle: 0,
            frames: frames.clone(),
            done,
        };
        let (source, _events) = open(&server.socket(), counted).unwrap();
        let recorder = Recorder::history(&server, NAME, &dir.path().join("history.wav"));
        server.link(NAME, recorder.node());
        finished.recv_timeout(4 * WITHIN).unwrap();
        assert_eq!(
            source.unsteered(),
            0,
            "cycles with no area to steer through"
        );
        let [plain, steered] = frames.map(|count| count.load(Ordering::Relaxed));
        #[expect(
            clippy::as_conversions,
            clippy::cast_precision_loss,
            reason = "frame counts far below the 53 bits a float holds exactly"
        )]
        let rise = (steered as f64 - plain as f64) / plain as f64;
        assert!(
            (rise - (RATIO - 1.0)).abs() <= RATIO_TOLERANCE,
            "{plain} frames at 1.0 and {steered} at {RATIO}: a rise of {rise:.6}"
        );
    }

    #[test]
    fn the_latency_parameter_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let (_source, _events) = open(&server.socket(), tone(1_000.0)).unwrap();
        let recorder = Recorder::superwhisper(&server, NAME, &dir.path().join("capture.wav"));
        server.link(NAME, recorder.node());
        let node = server.wait_node(NAME, Duration::ZERO);
        // An older library keeps the parameter to itself and shows an empty list, measured on 1.0.5.
        let (stated, times) = if states_latency(Path::new("/proc/self/maps")) {
            (json!([{"quantum": 0.0, "rate": 0, "ns": LATENCY_NS}]), 1)
        } else {
            (json!([]), 0)
        };
        assert_eq!(node.pointer("/info/params/ProcessLatency"), Some(&stated));
        // Stated once: no latency of the node or of a port repeats the figure.
        let dump = server.dump().to_string();
        assert_eq!(
            dump.matches(&LATENCY_NS.to_string()).count(),
            times,
            "{dump}"
        );
    }

    #[test]
    fn streaming_and_paused_follow_the_link() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let (_source, events) = open(&server.socket(), tone(1_000.0)).unwrap();
        let recorder = Recorder::superwhisper(&server, NAME, &dir.path().join("capture.wav"));
        server.link(NAME, recorder.node());
        assert_eq!(events.recv_timeout(WITHIN), Ok(Event::Streaming));
        drop(recorder.finish());
        assert_eq!(events.recv_timeout(WITHIN), Ok(Event::Paused));
    }

    #[test]
    fn the_server_dying_gives_lost() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let (source, events) = open(&server.socket(), tone(1_000.0)).unwrap();
        server.stop();
        assert_eq!(events.recv_timeout(WITHIN), Ok(Event::Lost));
        drop(source);
        // The drop ended the callbacks, so a closed channel with nothing in it is one loss and no more.
        assert_eq!(
            events.recv_timeout(WITHIN),
            Err(RecvTimeoutError::Disconnected)
        );
    }

    #[test]
    fn drop_while_streaming_leaves_no_thread() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let before = threads();
        let (source, events) = open(&server.socket(), tone(1_000.0)).unwrap();
        let recorder = Recorder::superwhisper(&server, NAME, &dir.path().join("capture.wav"));
        server.link(NAME, recorder.node());
        assert_eq!(events.recv_timeout(WITHIN), Ok(Event::Streaming));
        recorder.wait_frames(frames(LEAD, recorder.rate()), WITHIN);
        assert!(threads() > before, "the source ran no thread of its own");
        drop(source);
        // The drop joined its threads; the kernel takes a joined thread off the list a moment later.
        let dropped = Instant::now();
        while threads() != before {
            assert!(
                dropped.elapsed() < WITHIN,
                "{} threads, {before} before",
                threads()
            );
            thread::yield_now();
        }
        // A drop is not a loss: the channel closed with nothing more in it.
        assert_eq!(
            events.recv_timeout(WITHIN),
            Err(RecvTimeoutError::Disconnected)
        );
    }

    #[test]
    fn open_with_no_server_fails_inside_the_bound() {
        let dir = tempfile::tempdir().unwrap();
        let closed = dir.path().join("closed");
        drop(UnixListener::bind(&closed).unwrap());
        let absent = dir.path().join("absent");
        let full = dir.path().join("full");
        let listener = UnixListener::bind(&full).unwrap();
        // A backlog of zero holds one connection that nobody accepts, so the next one finds it full.
        rustix::net::listen(&listener, 0).unwrap();
        let _held = UnixStream::connect(&full).unwrap();
        for (socket, kind) in [
            (absent, ErrorKind::NotFound),
            (closed, ErrorKind::ConnectionRefused),
            (full, ErrorKind::WouldBlock),
        ] {
            let started = Instant::now();
            let Err(OpenError::NoServer(error)) = open(&socket, tone(1_000.0)) else {
                panic!("a source opened on {}", socket.display());
            };
            assert_eq!(error.kind(), kind);
            assert!(started.elapsed() < Duration::from_millis(PW_OPEN_MS));
        }
    }

    #[test]
    fn a_name_the_library_cannot_hold_fails_before_any_contact() {
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("absent");
        let spec = Spec {
            name: "pods\0source",
            ..spec(&absent)
        };
        let Err(OpenError::Library { call, error }) =
            Source::open(&spec, Box::new(tone(1_000.0)), Box::new(|_| {}))
        else {
            panic!("a node name with a NUL in it was taken");
        };
        assert_eq!(call, "pw_properties_new_dict");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(error.to_string().starts_with("\"node.name\": "), "{error}");
    }

    #[test]
    fn a_server_that_never_answers_fails_at_the_bound() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("mute");
        let _mute = UnixListener::bind(&socket).unwrap();
        let bound = Duration::from_millis(PW_OPEN_MS);
        let started = Instant::now();
        let Err(OpenError::NoServer(error)) = open(&socket, tone(1_000.0)) else {
            panic!("a source opened on a server that says nothing");
        };
        let waited = started.elapsed();
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert!(waited >= bound, "gave up after {waited:?}");
        assert!(waited < bound + OPEN_SLACK, "gave up after {waited:?}");
    }

    #[test]
    fn a_server_that_hangs_up_gives_no_server() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("hangs-up");
        let listener = UnixListener::bind(&socket).unwrap();
        let hang_up = thread::spawn(move || drop(listener.accept().unwrap()));
        let started = Instant::now();
        let result = open(&socket, tone(1_000.0));
        let waited = started.elapsed();
        let Err(OpenError::NoServer(error)) = result else {
            panic!("a source opened on a server that hung up");
        };
        assert_eq!(error.kind(), ErrorKind::ConnectionAborted, "{error}");
        // Joined only now: an `open` that never connected leaves the peer in `accept` for good.
        hang_up.join().unwrap();
        assert!(
            waited < Duration::from_millis(PW_OPEN_MS),
            "after {waited:?}"
        );
    }

    #[test]
    fn a_server_that_cannot_make_the_node_gives_refused() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start_without_client_nodes(dir.path());
        let error = open(&server.socket(), tone(1_000.0)).unwrap_err();
        assert!(matches!(error, OpenError::Refused(_)), "{error}");
    }

    #[test]
    fn a_remote_in_the_environment_is_ignored() {
        let (named_dir, other_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let named = Server::start(named_dir.path());
        let other = Server::start(other_dir.path());
        #[expect(
            unsafe_code,
            clippy::disallowed_methods,
            reason = "the variable is what this test is about"
        )]
        // SAFETY: nextest gives this test a process of its own, and no other thread of it reads the environment yet.
        unsafe {
            std::env::set_var("PIPEWIRE_REMOTE", other.socket());
        }
        let (_source, _events) = open(&named.socket(), tone(1_000.0)).unwrap();
        named.wait_node(NAME, Duration::ZERO);
        assert_eq!(node_names(&other.dump()), [DRIVER_NODE]);
    }
}
