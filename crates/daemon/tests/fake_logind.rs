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
    use zbus::Message;

    use super::support::bus::Listener;
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

    fn logind_owner(listener: &Listener) -> String {
        listener.ask_bus("GetNameOwner", &LOGIND_NAME)
    }

    fn logind_owned(listener: &Listener) -> bool {
        listener.ask_bus("NameHasOwner", &LOGIND_NAME)
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
        assert_eq!(sender(&signal), logind_owner(&listener));
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
        assert_ne!(sender(&forged), logind_owner(&everyone));
        rig.logind().prepare_for_sleep(false);
        let genuine = daemon_rule.next();
        assert!(
            !going_down(&genuine),
            "the daemon's rule let the impostor's signal through"
        );
        assert_eq!(sender(&genuine), logind_owner(&daemon_rule));
        rig.finish();
    }

    #[test]
    fn a_dropped_name_has_no_owner() {
        let rig = Rig::new();
        let listener = Listener::subscribe(&rig.system_bus_address(), &from_logind());
        assert!(logind_owned(&listener));
        rig.logind().drop_name();
        assert!(!logind_owned(&listener));
        rig.finish();
    }
}
