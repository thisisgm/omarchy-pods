//! End to end: the stand-in `trap-dial` runs under the trap, and each test reads the lines it prints.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod support;

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::process::{Command, ExitStatus, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant};

    use pods_proto::addr::BdAddr;
    use pods_proto::packets::{ATT_PSM, CONTROL_PSM};
    use pods_sys::l2cap::{Channel, Received};
    use pods_trap::{Answer, Outcome, Route, STOP_WAIT, Trap, TrapError, spawn};
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    use rustix::io::Errno;
    use rustix::net::{AddressFamily, RecvFlags, SocketAddrUnix, SocketFlags, SocketType};

    use super::support::{Reaped, STAND_IN};

    const WAIT: Duration = Duration::from_secs(5);
    /// What a loaded host may add to a delay the trap keeps.
    const REAL_TIME_SLACK: Duration = Duration::from_millis(500);
    const DEVICE: &str = "02:00:00:00:00:01";

    fn addr(text: &str) -> BdAddr {
        text.parse().unwrap()
    }

    fn stand_in(args: &[&str]) -> Command {
        let mut command = Command::new(STAND_IN);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        command
    }

    fn psm_arg(psm: u16) -> String {
        format!("{psm:#06x}")
    }

    fn start(args: &[&str], routes: Vec<Route>) -> (Reaped, Trap) {
        let (child, trap) = spawn(stand_in(args), routes).unwrap();
        (Reaped(child), trap)
    }

    /// Every channel `serve` gets goes into the returned receiver.
    fn into_channel(after: Duration) -> (Answer, mpsc::Receiver<Channel>) {
        let (sender, channels) = mpsc::channel();
        let serve = Arc::new(move |channel| {
            // A test that has stopped listening has already failed on its own wait.
            let _sent = sender.send(channel);
        });
        (Answer::Accept { after, serve }, channels)
    }

    /// An `Accept` whose `serve` only records that it ran.
    fn accept_watched(after: Duration) -> (Answer, Arc<AtomicBool>) {
        let served = Arc::new(AtomicBool::new(false));
        let serve = Arc::new({
            let served = Arc::clone(&served);
            move |_| served.store(true, Ordering::SeqCst)
        });
        (Answer::Accept { after, serve }, served)
    }

    fn route(device: &str, psm: u16, answer: Answer) -> Route {
        Route {
            device: addr(device),
            psm,
            answer,
        }
    }

    fn dial_routed(device: &str, psm: u16) -> (Reaped, Trap, mpsc::Receiver<Channel>) {
        let (answer, channels) = into_channel(Duration::ZERO);
        let (child, trap) = start(
            &["dial", device, &psm_arg(psm)],
            vec![route(device, psm, answer)],
        );
        (child, trap, channels)
    }

    fn expect_ping(far: &Channel) {
        far.set_recv_timeout(WAIT).unwrap();
        let mut buf = [0; 16];
        assert_eq!(
            far.recv(&mut buf).unwrap(),
            Received::Sdu(4),
            "waited for `ping`"
        );
        assert_eq!(&buf[..4], b"ping");
    }

    fn echo(far: &Channel) {
        expect_ping(far);
        far.send(b"pong").unwrap();
    }

    /// The stand-in's next line, or an empty text once it closed its output.
    fn next_line(child: &mut Reaped, what: &str) -> String {
        let stdout = child.0.stdout.as_mut().unwrap();
        let deadline = Instant::now().checked_add(WAIT).unwrap();
        let mut line = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "waited {WAIT:?} for {what}; got {:?}",
                String::from_utf8_lossy(&line)
            );
            let timeout = Timespec::try_from(remaining).unwrap();
            if poll(&mut [PollFd::new(&*stdout, PollFlags::IN)], Some(&timeout)).unwrap() == 0 {
                continue;
            }
            let mut byte = [0];
            if stdout.read(&mut byte).unwrap() == 0 || byte[0] == b'\n' {
                return String::from_utf8(line).unwrap();
            }
            line.push(byte[0]);
        }
    }

    fn first_line(mut child: Reaped) -> String {
        next_line(&mut child, "the stand-in's first line")
    }

    fn exit_status(child: &mut Reaped) -> ExitStatus {
        assert_eq!(next_line(child, "the stand-in to close its output"), "");
        child.0.wait().unwrap()
    }

    /// The stand-in with no trap at all.
    fn run_untrapped(args: &[&str]) -> String {
        first_line(Reaped(stand_in(args).spawn().unwrap()))
    }

    #[test]
    fn a_routed_dial_gets_a_channel_and_bytes_pass_both_ways() {
        let (child, trap, channels) = dial_routed(DEVICE, CONTROL_PSM);
        let far = channels
            .recv_timeout(WAIT)
            .expect("the far end of the trapped dial");
        echo(&far);
        assert_eq!(first_line(child), "ok 706f6e67");
        let dials = trap.dials();
        assert_eq!(dials.len(), 1, "{dials:?}");
        assert_eq!(dials[0].device, addr(DEVICE));
        assert_eq!(dials[0].psm, CONTROL_PSM);
        assert_eq!(dials[0].outcome, Outcome::Accepted);
        assert_eq!(trap.sockets(), 1);
        assert_eq!(trap.finish(), Ok(dials));
    }

    #[test]
    fn the_record_holds_each_dial_in_order() {
        let (control, channels) = into_channel(Duration::ZERO);
        let args = ["twice", DEVICE, &psm_arg(CONTROL_PSM), &psm_arg(ATT_PSM)];
        let (mut child, trap) = start(&args, vec![route(DEVICE, CONTROL_PSM, control)]);
        let (att, att_channels) = into_channel(Duration::ZERO);
        trap.route(addr(DEVICE), ATT_PSM, att);
        echo(
            &channels
                .recv_timeout(WAIT)
                .expect("the far end of the first dial"),
        );
        assert_eq!(
            next_line(&mut child, "the first dial's line"),
            "ok 706f6e67"
        );
        echo(
            &att_channels
                .recv_timeout(WAIT)
                .expect("the far end of the second dial"),
        );
        assert_eq!(
            next_line(&mut child, "the second dial's line"),
            "ok 706f6e67"
        );
        let dials = trap.finish().unwrap();
        let psms: Vec<u16> = dials.iter().map(|dial| dial.psm).collect();
        assert_eq!(psms, [CONTROL_PSM, ATT_PSM]);
        assert!(dials[0].at <= dials[1].at, "{dials:?}");
        assert!(
            dials.iter().all(|dial| dial.outcome == Outcome::Accepted),
            "{dials:?}"
        );
    }

    #[test]
    fn a_route_taken_away_is_host_down_again() {
        let (answer, channels) = into_channel(Duration::ZERO);
        let psm = psm_arg(CONTROL_PSM);
        let (mut child, trap) = start(
            &["twice", DEVICE, &psm, &psm],
            vec![route(DEVICE, CONTROL_PSM, answer)],
        );
        let far = channels
            .recv_timeout(WAIT)
            .expect("the far end of the first dial");
        expect_ping(&far);
        trap.unroute(addr(DEVICE), CONTROL_PSM);
        far.send(b"pong").unwrap();
        assert_eq!(
            next_line(&mut child, "the first dial's line"),
            "ok 706f6e67"
        );
        assert_eq!(
            next_line(&mut child, "the second dial's line"),
            "error connect 112"
        );
        let outcomes: Vec<Outcome> = trap.dials().into_iter().map(|dial| dial.outcome).collect();
        assert_eq!(outcomes, [Outcome::Accepted, Outcome::Unrouted]);
        assert!(matches!(trap.finish(), Err(TrapError::Violations { .. })));
    }

    #[test]
    fn a_refusal_gives_the_child_the_errno_of_the_route() {
        for errno in [Errno::CONNREFUSED, Errno::HOSTDOWN, Errno::TIMEDOUT] {
            let refuse = Answer::Refuse {
                after: Duration::ZERO,
                errno,
            };
            let (child, trap) = start(
                &["dial", DEVICE, &psm_arg(CONTROL_PSM)],
                vec![route(DEVICE, CONTROL_PSM, refuse)],
            );
            assert_eq!(
                first_line(child),
                format!("error connect {}", errno.raw_os_error())
            );
            let outcomes: Vec<Outcome> = trap
                .finish()
                .unwrap()
                .into_iter()
                .map(|dial| dial.outcome)
                .collect();
            assert_eq!(outcomes, [Outcome::Refused(errno)]);
        }
    }

    #[test]
    fn an_answer_waits_its_delay() {
        let delay = Duration::from_millis(300);
        let refuse = Answer::Refuse {
            after: delay,
            errno: Errno::CONNREFUSED,
        };
        let (mut child, trap) = start(
            &["dial", DEVICE, &psm_arg(CONTROL_PSM)],
            vec![route(DEVICE, CONTROL_PSM, refuse)],
        );
        let trapped = trap.wait_dials(1, WAIT).expect("the dial to be trapped")[0].at;
        assert_eq!(
            next_line(&mut child, "the refused dial's line"),
            "error connect 111"
        );
        let took = trapped.elapsed();
        assert!(
            took >= delay && took <= delay + REAL_TIME_SLACK,
            "the answer took {took:?}"
        );
    }

    #[test]
    fn an_unrouted_connect_is_host_down_and_fails_the_finish() {
        let (child, trap) = start(
            &["dial", "02:00:00:00:00:09", &psm_arg(CONTROL_PSM)],
            Vec::new(),
        );
        assert_eq!(first_line(child), "error connect 112");
        let error = trap.finish().unwrap_err();
        assert!(matches!(error, TrapError::Violations { .. }), "{error:?}");
        let text = error.to_string();
        for wanted in ["connect", "02:00:00:00:00:09", "0x1001", "EHOSTDOWN"] {
            assert!(text.contains(wanted), "no `{wanted}` in {text}");
        }
    }

    #[test]
    fn a_nonblocking_socket_gets_a_nonblocking_end() {
        let (child, trap) = start(&["socket"], Vec::new());
        assert_eq!(first_line(child), "ok");
        assert_eq!(trap.sockets(), 1);
        assert_eq!(trap.finish(), Ok(Vec::new()));
    }

    #[test]
    fn another_bluetooth_socket_is_refused_and_fails_the_finish() {
        let (child, trap) = start(&["raw"], Vec::new());
        assert_eq!(first_line(child), "error socket 93");
        let error = trap.finish().unwrap_err();
        assert!(matches!(error, TrapError::Violations { .. }), "{error:?}");
        let text = error.to_string();
        for wanted in ["socket", "type 3", "protocol 1"] {
            assert!(text.contains(wanted), "no `{wanted}` in {text}");
        }
    }

    #[test]
    fn a_connect_the_trap_does_not_own_continues_untouched() {
        // Eleven bytes, so the address is 14 bytes long like an L2CAP one and the filter hands it over.
        let name = format!("trap{:07}", std::process::id() % 10_000_000);
        let listener = rustix::net::socket_with(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )
        .unwrap();
        rustix::net::bind(
            &listener,
            &SocketAddrUnix::new_abstract_name(name.as_bytes()).unwrap(),
        )
        .unwrap();
        rustix::net::listen(&listener, 1).unwrap();
        let (child, trap) = start(&["unix", &name], Vec::new());
        let timeout = Timespec::try_from(WAIT).unwrap();
        let ready = poll(&mut [PollFd::new(&listener, PollFlags::IN)], Some(&timeout)).unwrap();
        assert_eq!(ready, 1, "waited for the stand-in to connect");
        let accepted = rustix::net::accept(&listener).unwrap();
        let mut buf = [0; 16];
        let ready = poll(&mut [PollFd::new(&accepted, PollFlags::IN)], Some(&timeout)).unwrap();
        assert_eq!(ready, 1, "waited for `ping`");
        let (read, _) = rustix::net::recv(&accepted, &mut buf, RecvFlags::DONTWAIT).unwrap();
        assert_eq!(&buf[..read], b"ping");
        assert_eq!(first_line(child), "ok");
        assert_eq!(trap.dials(), []);
        assert_eq!(trap.finish(), Ok(Vec::new()));
    }

    #[test]
    fn a_child_killed_inside_a_call_ends_the_trap_cleanly() {
        let answer_delay = Duration::from_secs(5);
        // Below `answer_delay`, so a `finish` that waited the answer out fails here.
        let test_bound = Duration::from_secs(3);
        let test_started = Instant::now();
        let (accept, served) = accept_watched(answer_delay);
        let (mut child, trap) = start(
            &["dial", DEVICE, &psm_arg(CONTROL_PSM)],
            vec![route(DEVICE, CONTROL_PSM, accept)],
        );
        let dials = trap.wait_dials(1, WAIT).expect("the dial to be trapped");
        assert_eq!(dials[0].outcome, Outcome::Pending);
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        let finishing = Instant::now();
        let dials = trap.finish().unwrap();
        assert!(
            finishing.elapsed() < STOP_WAIT,
            "finish took {:?}",
            finishing.elapsed()
        );
        assert_eq!(dials[0].outcome, Outcome::Abandoned);
        assert!(
            !served.load(Ordering::SeqCst),
            "serve ran for a dead caller"
        );
        assert!(
            test_started.elapsed() < test_bound,
            "the test took {:?}",
            test_started.elapsed()
        );
    }

    #[test]
    fn a_caller_killed_before_its_answer_is_abandoned_and_never_served() {
        let delay = Duration::from_millis(300);
        let (accept, served) = accept_watched(delay);
        let (mut child, trap) = start(
            &["dial", DEVICE, &psm_arg(CONTROL_PSM)],
            vec![route(DEVICE, CONTROL_PSM, accept)],
        );
        trap.wait_dials(1, WAIT).expect("the dial to be trapped");
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        // A second dial never comes, so this waits out the delay and the trap answers a caller already gone.
        let outlived = trap.wait_dials(2, delay + REAL_TIME_SLACK).unwrap_err();
        assert!(
            matches!(outlived, TrapError::Deadline { .. }),
            "{outlived:?}"
        );
        let answered = trap.dials();
        assert_eq!(answered[0].outcome, Outcome::Abandoned, "{answered:?}");
        assert_eq!(trap.finish(), Ok(answered));
        assert!(
            !served.load(Ordering::SeqCst),
            "serve ran for a dead caller"
        );
    }

    #[test]
    fn a_wait_for_a_dial_that_never_comes_ends_at_its_deadline() {
        let deadline = Duration::from_millis(200);
        let (_child, trap) = start(&["idle"], Vec::new());
        let before = Instant::now();
        let error = trap.wait_dials(1, deadline).unwrap_err();
        let took = before.elapsed();
        assert!(
            took >= deadline && took <= deadline + REAL_TIME_SLACK,
            "the wait took {took:?}"
        );
        assert!(
            matches!(error, TrapError::Deadline { wanted: 1, .. }),
            "{error:?}"
        );
        let text = error.to_string();
        assert!(text.contains("for 1 dial and none came"), "{text}");
    }

    #[test]
    fn a_call_after_the_trap_is_gone_fails_with_enosys_and_the_child_lives() {
        let (answer, _channels) = into_channel(Duration::ZERO);
        let (mut child, trap) = start(
            &["late", DEVICE, &psm_arg(CONTROL_PSM)],
            vec![route(DEVICE, CONTROL_PSM, answer)],
        );
        drop(trap);
        child.0.stdin.as_mut().unwrap().write_all(b"go\n").unwrap();
        assert_eq!(
            next_line(&mut child, "the late dial's line"),
            "error socket 38"
        );
        assert!(exit_status(&mut child).success());
    }

    #[test]
    #[expect(
        clippy::print_stdout,
        reason = "the hosted log records which call of the untrapped dial failed"
    )]
    fn a_machine_without_a_radio_still_gets_a_channel() {
        // Only the hosted runner runs the control; elsewhere an untrapped dial could reach a real radio.
        if std::env::var_os("LIBREPODS_TRAP_EXPECT_NO_RADIO").is_some() {
            let line = run_untrapped(&["dial", DEVICE, &psm_arg(CONTROL_PSM)]);
            println!("untrapped dial: {line}");
            assert!(
                line.starts_with("error socket ") || line.starts_with("error connect "),
                "an untrapped dial succeeded ({line:?}), so a radio answered and the run proves nothing"
            );
        }
        let (child, trap, channels) = dial_routed(DEVICE, CONTROL_PSM);
        let far = channels
            .recv_timeout(WAIT)
            .expect("the far end of the trapped dial");
        echo(&far);
        assert_eq!(first_line(child), "ok 706f6e67");
        assert_eq!(trap.finish().map(|dials| dials.len()), Ok(1));
    }
}
