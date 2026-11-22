//! Support tests: the fake media players on the rig's private session bus, driven by a client as the daemon will drive them.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "these tests drive the fake players alone, so the daemon helpers the other suites share go unused"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::thread;
    use std::time::Duration;

    use zbus::{DBusError, Message, OwnedVariant};

    use super::support::bus::Listener;
    use super::support::players::{
        Asked, FakePlayer, PLAYER_INTERFACE, PLAYER_PATH, PROPERTIES, Playback, STATUS_PROPERTY,
        UNKNOWN_METHOD,
    };
    use super::support::rig::Rig;

    /// A call on a private bus arrives in milliseconds; one later than this is lost.
    const WITHIN: Duration = Duration::from_secs(5);
    /// Long enough for a misrouted call on a private bus to have arrived.
    const QUIET: Duration = Duration::from_millis(200);

    /// Every status change the player announces, as a product that listens would subscribe.
    fn changes_of(player: &FakePlayer) -> String {
        format!(
            "type='signal',sender='{}',path='{PLAYER_PATH}',interface='{PROPERTIES}',member='PropertiesChanged'",
            player.bus_name()
        )
    }

    fn call(listener: &Listener, player: &FakePlayer, member: &str) -> zbus::Result<Message> {
        listener.call(
            player.bus_name(),
            PLAYER_PATH,
            PLAYER_INTERFACE,
            member,
            &(),
        )
    }

    /// The status as a product reads it, through `Properties.Get`.
    fn status_on_bus(listener: &Listener, player: &FakePlayer) -> String {
        let reply = listener
            .call(
                player.bus_name(),
                PLAYER_PATH,
                PROPERTIES,
                "Get",
                &(PLAYER_INTERFACE, STATUS_PROPERTY),
            )
            .unwrap();
        let status: OwnedVariant = reply.body().deserialize().unwrap();
        String::try_from(status).unwrap()
    }

    /// The status in the next `PropertiesChanged`, failing unless `player` sent it from its object in this shape.
    fn next_announced(listener: &mut Listener, player: &FakePlayer) -> String {
        let signal = listener.next();
        let owner: String = listener.ask_bus("GetNameOwner", &player.bus_name());
        assert_eq!(signal.header().sender().unwrap().as_str(), owner);
        assert_eq!(signal.header().path().unwrap().as_str(), PLAYER_PATH);
        let (interface, changed, invalidated): (
            String,
            HashMap<String, OwnedVariant>,
            Vec<String>,
        ) = signal.body().deserialize().unwrap();
        assert_eq!(interface, PLAYER_INTERFACE);
        assert!(invalidated.is_empty(), "invalidated: {invalidated:?}");
        assert_eq!(changed.len(), 1, "changed: {changed:?}");
        String::try_from(changed[STATUS_PROPERTY].try_clone().unwrap()).unwrap()
    }

    fn error_name(result: zbus::Result<Message>) -> String {
        match result {
            Err(zbus::Error::MethodError(name, _, _)) => name.to_string(),
            // zbus turns an error of the `org.freedesktop.DBus.Error` family into this variant.
            Err(zbus::Error::FDO(error)) => error.name().to_string(),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    fn names(listener: &Listener) -> Vec<String> {
        listener.ask_bus("ListNames", &())
    }

    #[test]
    fn pause_and_play_change_the_status_and_are_recorded() {
        let rig = Rig::new();
        let player = rig.player("first", Playback::Playing);
        let mut listener = Listener::subscribe(rig.session_bus(), &changes_of(&player));
        assert_eq!(status_on_bus(&listener, &player), "Playing");
        call(&listener, &player, "Pause").unwrap();
        assert_eq!(player.playback(), Playback::Paused);
        assert_eq!(next_announced(&mut listener, &player), "Paused");
        assert_eq!(status_on_bus(&listener, &player), "Paused");
        // A second pause changes nothing, so the next announcement is the play.
        call(&listener, &player, "Pause").unwrap();
        call(&listener, &player, "Play").unwrap();
        assert_eq!(next_announced(&mut listener, &player), "Playing");
        player.set(Playback::Stopped);
        assert_eq!(next_announced(&mut listener, &player), "Stopped");
        call(&listener, &player, "Pause").unwrap();
        assert_eq!(player.playback(), Playback::Stopped);
        call(&listener, &player, "Play").unwrap();
        assert_eq!(next_announced(&mut listener, &player), "Playing");
        assert_eq!(player.playback(), Playback::Playing);
        assert_eq!(
            player.asked(),
            [
                Asked::Pause,
                Asked::Pause,
                Asked::Play,
                Asked::Pause,
                Asked::Play
            ]
        );
        assert!(player.refused().is_empty(), "{:?}", player.refused());
        player.leave();
        rig.finish();
    }

    #[test]
    fn a_held_player_answers_after_release() {
        let rig = Rig::new();
        let player = rig.player("first", Playback::Playing);
        let mut listener = Listener::subscribe(rig.session_bus(), &changes_of(&player));
        player.hold_replies(true);
        let address = rig.session_bus();
        let reply = thread::scope(|scope| {
            let caller = scope.spawn(|| {
                let client = Listener::subscribe(address, &changes_of(&player));
                call(&client, &player, "Pause")
            });
            player.wait_asked(Asked::Pause, WITHIN);
            assert_eq!(player.playback(), Playback::Playing);
            assert!(!caller.is_finished(), "a held call was answered");
            player.hold_replies(false);
            caller.join().unwrap()
        });
        reply.unwrap();
        assert_eq!(player.playback(), Playback::Paused);
        assert_eq!(next_announced(&mut listener, &player), "Paused");
        player.leave();
        rig.finish();
    }

    #[test]
    fn waits_for_each_kind_ignore_the_other_kind() {
        let rig = Rig::new();
        let player = rig.player("first", Playback::Playing);
        let listener = Listener::subscribe(rig.session_bus(), &changes_of(&player));
        call(&listener, &player, "Play").unwrap();
        call(&listener, &player, "Pause").unwrap();
        player.wait_asked(Asked::Pause, WITHIN);
        player.wait_asked(Asked::Play, WITHIN);
        player.leave();
        rig.finish();
    }

    #[test]
    #[should_panic(expected = "Pause not asked of org.mpris.MediaPlayer2.first within 200ms")]
    fn a_wait_returns_once_per_call() {
        let rig = Rig::new();
        let player = rig.player("first", Playback::Playing);
        let listener = Listener::subscribe(rig.session_bus(), &changes_of(&player));
        call(&listener, &player, "Pause").unwrap();
        player.wait_asked(Asked::Pause, WITHIN);
        player.wait_asked(Asked::Pause, QUIET);
    }

    #[test]
    fn introspection_is_refused() {
        let rig = Rig::new();
        let player = rig.player("first", Playback::Playing);
        let listener = Listener::subscribe(rig.session_bus(), &changes_of(&player));
        let introspect = listener.call(
            player.bus_name(),
            PLAYER_PATH,
            "org.freedesktop.DBus.Introspectable",
            "Introspect",
            &(),
        );
        assert_eq!(error_name(introspect), UNKNOWN_METHOD);
        let get_all = listener.call(
            player.bus_name(),
            PLAYER_PATH,
            PROPERTIES,
            "GetAll",
            &(PLAYER_INTERFACE,),
        );
        assert_eq!(error_name(get_all), UNKNOWN_METHOD);
        assert_eq!(status_on_bus(&listener, &player), "Playing");
        assert_eq!(
            player.refused(),
            [
                "org.freedesktop.DBus.Introspectable.Introspect",
                "org.freedesktop.DBus.Properties.GetAll"
            ]
        );
        assert!(player.asked().is_empty(), "{:?}", player.asked());
        player.leave();
        rig.finish();
    }

    #[test]
    fn a_player_that_leaves_is_gone_from_the_name_list() {
        let rig = Rig::new();
        let leaving = rig.player("first", Playback::Playing);
        let dropped = rig.player("second", Playback::Paused);
        let first = leaving.bus_name().to_owned();
        let second = dropped.bus_name().to_owned();
        let listener = Listener::subscribe(rig.session_bus(), &changes_of(&leaving));
        let before = names(&listener);
        assert!(before.contains(&first), "{before:?}");
        assert!(before.contains(&second), "{before:?}");
        leaving.leave();
        let after_leave = names(&listener);
        assert!(!after_leave.contains(&first), "{after_leave:?}");
        assert!(after_leave.contains(&second), "{after_leave:?}");
        drop(dropped);
        let after_drop = names(&listener);
        assert!(!after_drop.contains(&second), "{after_drop:?}");
        rig.finish();
    }

    #[test]
    fn two_players_on_one_bus_are_told_apart() {
        let rig = Rig::new();
        let first = rig.player("first", Playback::Playing);
        let second = rig.player("second", Playback::Paused);
        let mut listener = Listener::subscribe(rig.session_bus(), &changes_of(&first));
        call(&listener, &first, "Pause").unwrap();
        second.none_for(QUIET);
        assert_eq!(first.asked(), [Asked::Pause]);
        call(&listener, &second, "Play").unwrap();
        first.none_for(QUIET);
        assert_eq!(first.asked(), [Asked::Pause]);
        assert_eq!(second.asked(), [Asked::Play]);
        assert_eq!(first.playback(), Playback::Paused);
        assert_eq!(second.playback(), Playback::Playing);
        assert_eq!(status_on_bus(&listener, &first), "Paused");
        assert_eq!(status_on_bus(&listener, &second), "Playing");
        assert_eq!(next_announced(&mut listener, &first), "Paused");
        // The second player's change comes first, so a leak of it would fail the sender check.
        second.set(Playback::Stopped);
        first.set(Playback::Playing);
        assert_eq!(next_announced(&mut listener, &first), "Playing");
        first.leave();
        second.leave();
        rig.finish();
    }
}
