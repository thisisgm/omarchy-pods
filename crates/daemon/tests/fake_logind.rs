//! Support tests: the fake logind on the rig's private system bus, seen by a listener as the daemon will see it.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "these tests drive the fake logind alone, so the daemon helpers the other suites share go unused"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::future::poll_fn;
    use std::pin::Pin;

    use tokio::runtime::{Builder as RuntimeBuilder, Runtime};
    use zbus::connection::Builder;
    use zbus::export::futures_core::Stream;
    use zbus::export::serde::de::DeserializeOwned;
    use zbus::zvariant::Type;
    use zbus::{Connection, Message, MessageStream};

    use super::support::bus::in_time;
    use super::support::logind::{LOGIND_NAME, MANAGER_INTERFACE, MANAGER_PATH, SLEEP_SIGNAL};
    use super::support::rig::Rig;

    /// Every signal the owner of the logind name sends, as the daemon will subscribe.
    fn from_logind() -> String {
        format!("type='signal',sender='{LOGIND_NAME}'")
    }

    /// Every `PrepareForSleep` on the bus, whoever sends it.
    fn any_sleep() -> String {
        format!("type='signal',member='{SLEEP_SIGNAL}'")
    }

    /// A client of the private system bus whose match rule the bus confirmed before `subscribe` returns.
    struct Listener {
        runtime: Runtime,
        client: Connection,
        signals: MessageStream,
    }

    impl Listener {
        fn subscribe(address: &str, rule: &str) -> Self {
            let runtime = RuntimeBuilder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (client, signals) = in_time(&runtime, async {
                let client = Builder::address(address).build().await.unwrap();
                // The stream is returned only once the bus answered `AddMatch`.
                let signals = MessageStream::for_match_rule(rule, &client, None)
                    .await
                    .unwrap();
                (client, signals)
            });
            Self {
                runtime,
                client,
                signals,
            }
        }

        fn next(&mut self) -> Message {
            let signals = &mut self.signals;
            let next = poll_fn(|context| Pin::new(&mut *signals).poll_next(context));
            in_time(&self.runtime, next)
                .expect("the listener's stream ended")
                .unwrap()
        }

        /// Asks the bus daemon itself `method` about the logind name.
        fn ask_bus<T: DeserializeOwned + Type>(&self, method: &str) -> T {
            in_time(&self.runtime, async {
                let reply = self
                    .client
                    .call_method(
                        Some("org.freedesktop.DBus"),
                        "/org/freedesktop/DBus",
                        Some("org.freedesktop.DBus"),
                        method,
                        &LOGIND_NAME,
                    )
                    .await
                    .unwrap();
                reply.body().deserialize().unwrap()
            })
        }

        fn logind_owner(&self) -> String {
            self.ask_bus("GetNameOwner")
        }

        fn logind_owned(&self) -> bool {
            self.ask_bus("NameHasOwner")
        }
    }

    fn going_down(signal: &Message) -> bool {
        signal.body().deserialize().unwrap()
    }

    fn sender(signal: &Message) -> String {
        signal.header().sender().unwrap().to_string()
    }

    #[test]
    fn a_listener_on_the_private_bus_sees_both_edges_in_order() {
        let rig = Rig::new();
        let mut listener = Listener::subscribe(&rig.system_bus_address(), &from_logind());
        rig.logind().prepare_for_sleep(true);
        rig.logind().prepare_for_sleep(false);
        let first = listener.next();
        let second = listener.next();
        assert_eq!(first.header().member().unwrap().as_str(), SLEEP_SIGNAL);
        assert_eq!(second.header().member().unwrap().as_str(), SLEEP_SIGNAL);
        assert!(going_down(&first), "the first edge is the one into sleep");
        assert!(!going_down(&second), "the second edge is the wake");
        rig.finish();
    }

    #[test]
    fn the_signal_carries_the_manager_path_and_interface() {
        let rig = Rig::new();
        let mut listener = Listener::subscribe(&rig.system_bus_address(), &from_logind());
        rig.logind().prepare_for_sleep(true);
        let signal = listener.next();
        let header = signal.header();
        assert_eq!(header.path().unwrap().as_str(), MANAGER_PATH);
        assert_eq!(header.interface().unwrap().as_str(), MANAGER_INTERFACE);
        assert_eq!(header.member().unwrap().as_str(), SLEEP_SIGNAL);
        assert_eq!(signal.body().signature().to_string(), "b");
        assert_eq!(sender(&signal), listener.logind_owner());
        rig.finish();
    }

    #[test]
    fn an_impostor_signal_has_another_sender() {
        let rig = Rig::new();
        let impostor = rig.impostor();
        let mut everyone = Listener::subscribe(&rig.system_bus_address(), &any_sleep());
        let mut daemon_rule = Listener::subscribe(&rig.system_bus_address(), &from_logind());
        impostor.prepare_for_sleep(true);
        // The bus hands one signal to every matching listener at once, so this one's copy proves the other was judged.
        let forged = everyone.next();
        let header = forged.header();
        assert_eq!(header.path().unwrap().as_str(), MANAGER_PATH);
        assert_eq!(header.interface().unwrap().as_str(), MANAGER_INTERFACE);
        assert!(going_down(&forged));
        assert_ne!(sender(&forged), everyone.logind_owner());
        rig.logind().prepare_for_sleep(false);
        let genuine = daemon_rule.next();
        assert!(
            !going_down(&genuine),
            "the daemon's rule let the impostor's signal through"
        );
        assert_eq!(sender(&genuine), daemon_rule.logind_owner());
        rig.finish();
    }

    #[test]
    fn a_dropped_name_has_no_owner() {
        let rig = Rig::new();
        let listener = Listener::subscribe(&rig.system_bus_address(), &from_logind());
        assert!(listener.logind_owned());
        rig.logind().drop_name();
        assert!(!listener.logind_owned());
        rig.finish();
    }
}
