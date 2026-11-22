//! The built daemon opens its channel only through the kernel: sockets, variables and an inherited descriptor laid as lures all go unused.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "these tests drive the daemon through the rig, so the socket helpers the other suites share go unused"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::fd::OwnedFd;
    use std::path::PathBuf;
    use std::process::Stdio;
    use std::time::Duration;

    use pods_core::link::ladder::HELD_DELAYS_MS;
    use pods_fake::Profile;
    use pods_trap::Answer;
    use rustix::io::Errno;
    use rustix::net::{
        AddressFamily, RecvFlags, SocketAddrUnix, SocketFlags, SocketType, accept, bind, listen,
        recv, socket_with, socketpair,
    };

    use super::support::rig::{DIAL_WAIT, Rig, connected};
    use super::support::wait_within;

    const LURE_VARIABLES: [&str; 9] = [
        "LIBREPODS_PODS",
        "LIBREPODS_PODS_SOCKET",
        "LIBREPODS_CHANNEL",
        "LIBREPODS_CONNECT",
        "LIBREPODS_CONNECTOR",
        "LIBREPODS_DEVICE",
        "LIBREPODS_FAKE",
        "LIBREPODS_TEST",
        "LIBREPODS_L2CAP",
    ];

    /// Set on the hosted runner only, which has no Bluetooth in its kernel, so an untrapped dial cannot page a real device.
    const NO_RADIO: &str = "LIBREPODS_TRAP_EXPECT_NO_RADIO";

    /// Room for the start of anything the daemon might send on the inherited descriptor.
    const PEEK_LEN: usize = 64;

    /// Every way in a fake could take, each of which must see nothing.
    struct Lures {
        listeners: Vec<(PathBuf, OwnedFd)>,
        /// The far end of the descriptor the daemon inherits as its standard input.
        inherited: OwnedFd,
    }

    fn listening(path: PathBuf) -> (PathBuf, OwnedFd) {
        let flags = SocketFlags::CLOEXEC | SocketFlags::NONBLOCK;
        let listener =
            socket_with(AddressFamily::UNIX, SocketType::SEQPACKET, flags, None).unwrap();
        bind(&listener, &SocketAddrUnix::new(&path).unwrap()).unwrap();
        listen(&listener, 1).unwrap();
        (path, listener)
    }

    fn lay(rig: &mut Rig) -> Lures {
        let runtime_socket = rig.session().runtime_dir().join("librepods-pods.sock");
        let state_dir = rig.session().state_home().join("librepods");
        fs::create_dir(&state_dir).unwrap();
        let listeners = vec![
            listening(runtime_socket.clone()),
            listening(state_dir.join("pods.sock")),
        ];
        let (near, far) = socketpair(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )
        .unwrap();
        rig.prepare(move |command| {
            for variable in LURE_VARIABLES {
                command.env(variable, &runtime_socket);
            }
            command
                .env("LIBREPODS_CHANNEL_FD", "0")
                .stdin(Stdio::from(near));
        });
        Lures {
            listeners,
            inherited: far,
        }
    }

    impl Lures {
        fn check(self) {
            let mut peek = [0; PEEK_LEN];
            let sent = recv(&self.inherited, &mut peek, RecvFlags::DONTWAIT);
            assert_eq!(
                sent.err(),
                Some(Errno::WOULDBLOCK),
                "the daemon sent on its standard input"
            );
            for (path, listener) in &self.listeners {
                let accepted = accept(listener);
                assert_eq!(
                    accepted.err(),
                    Some(Errno::WOULDBLOCK),
                    "the lure at {} took a connection",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn the_binary_takes_a_channel_only_from_the_kernel() {
        let mut rig = Rig::new();
        let device = rig.add_pods(Profile::default());
        let lures = lay(&mut rig);
        rig.start();
        rig.answer(
            &device,
            Answer::Refuse {
                after: Duration::ZERO,
                errno: Errno::HOSTDOWN,
            },
        );
        rig.link_up(&device);
        let first_retry = Duration::from_millis(HELD_DELAYS_MS[0]);
        let dials = rig.wait_dials(2, DIAL_WAIT + first_retry);
        assert!(!connected(&rig.status()));
        assert_eq!(rig.sockets(), dials.len());
        lures.check();
        rig.finish();

        // Only the hosted runner runs the untrapped half; elsewhere an untrapped dial could reach a real radio.
        if std::env::var_os(NO_RADIO).is_some() {
            let mut rig = Rig::new();
            let device = rig.add_pods(Profile::default());
            let lures = lay(&mut rig);
            rig.start_untrapped();
            rig.link_up(&device);
            // The kernel refuses the family at once, and the second attempt leaves one held delay after the first.
            let attempt_limit = DIAL_WAIT + first_retry + DIAL_WAIT;
            wait_within("a second attempt", attempt_limit, || {
                let attempts = rig.status()["reconnect_attempts_total"].as_u64();
                (attempts == Some(1)).then_some(())
            });
            lures.check();
            rig.finish();
        }
    }
}
