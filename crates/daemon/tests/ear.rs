//! Ear detection through the built daemon: the fake pods report where the buds sit, and the fake players on the session bus are paused and resumed.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use pods_core::policy::{BOTH_OUT_SETTLE_MS, PAUSED_MAX};
    use pods_fake::Profile;
    use pods_samples::{CASE, LEFT, Placed, RIGHT, battery_of};

    use super::support::audio::wire::sink_named;
    use super::support::audio::{Asked as AudioAsked, FakeAudio, PERCENT};
    use super::support::players::{Asked, FakePlayer, Playback};
    use super::support::rig::{
        ANSWERED_WITHIN, Device, REAL_TIME_SLACK, Rig, assert_gap, connected, not_before,
    };
    use super::support::{HANG_LIMIT, pause, wait_for};

    const SETTLE: Duration = Duration::from_millis(BOTH_OUT_SETTLE_MS);
    /// Two flaps end well before the first settle, and leave time to change the preference before the last one ends.
    const FLAP: Duration = Duration::from_millis(BOTH_OUT_SETTLE_MS * 3 / 8);
    /// The daemon's bound on one player's answers, from `PLAYER_CALL_TIMEOUT_MS` in `src/daemon/media.rs`.
    const CALL_TIMEOUT: Duration = Duration::from_millis(500);
    /// The most players the daemon asks, `PLAYERS_MAX` in `src/daemon/media.rs`, which is the policy's own bound.
    const PLAYERS_MAX: usize = PAUSED_MAX;
    const PLAYERS_ON_A_BUSY_BUS: usize = 60;
    /// Long enough for a call the daemon was about to make to reach a player on a private bus.
    const QUIET: Duration = REAL_TIME_SLACK;
    /// From a placement to the call it causes, on a loaded host.
    const PROMPT: Duration = REAL_TIME_SLACK;
    /// One entry of a battery packet, `kind 01 level status 01`, as `pods_samples` writes them right, left, case.
    const BATTERY_ENTRY: usize = 5;
    const BATTERY_ENTRIES_LEN: usize = 3 * BATTERY_ENTRY;
    const TWO_ENTRIES: usize = 2 * BATTERY_ENTRY;
    const SPEAKERS: &str = "alsa_output.speakers";

    use Placed::{InEar, OutOfEar};

    /// Sets the preference with `librepods set` and waits for the status to show its number.
    fn prefer(rig: &Rig, word: &str, number: u64) {
        let output = rig.client(&["set", "ear_detection", word]);
        assert!(
            output.status.success(),
            "set ear_detection {word}: {output:?}"
        );
        rig.wait_status(&format!("ear detection {word}"), |status| {
            status["ear_detection_behavior"] == number
        });
    }

    /// Places the buds, primary then secondary, and waits for the status to show it; the primary is the right bud.
    fn place(rig: &Rig, device: &Device, right: Placed, left: Placed) {
        device.pods.set_ear(right, left);
        wait_worn(rig, right == InEar, left == InEar);
    }

    fn wait_worn(rig: &Rig, right: bool, left: bool) {
        rig.wait_status(&format!("right in ear {right}, left {left}"), |status| {
            status["right"]["in_ear"] == right && status["left"]["in_ear"] == left
        });
    }

    /// Places the buds and gives the time it took `player` to receive `call`.
    fn placed_until(
        device: &Device,
        right: Placed,
        left: Placed,
        player: &FakePlayer,
        call: Asked,
        within: Duration,
    ) -> (Instant, Instant) {
        let placed = Instant::now();
        device.pods.set_ear(right, left);
        player.wait_asked(call, within);
        (placed, Instant::now())
    }

    /// Fails if `player` is asked anything before `until`, or within `QUIET` if that is later.
    fn quiet_until(player: &FakePlayer, until: Instant) {
        let left = until.saturating_duration_since(Instant::now());
        player.none_for(left.max(QUIET));
    }

    /// Two volume changes, then the refresh after each: the second is asked only once the first was told to the driver.
    fn told(audio: &FakeAudio, sink: &str) {
        for step in 1..=2 {
            let before = audio.requests().len();
            audio.set_volume(sink, PERCENT.saturating_add(step));
            wait_for("a refresh after the volume change", || {
                audio.requests()[before..]
                    .contains(&AudioAsked::ServerInfo)
                    .then_some(())
            });
        }
    }

    /// The battery packet with the left bud first, which makes it the primary.
    fn left_first_battery() -> Vec<u8> {
        let mut packet = battery_of(LEFT, RIGHT, CASE);
        let entries = packet.len().checked_sub(BATTERY_ENTRIES_LEN).unwrap();
        packet[entries..][..TWO_ENTRIES].rotate_left(BATTERY_ENTRY);
        packet
    }

    #[test]
    fn one_bud_out_pauses_the_playing_player_and_both_in_resumes_it() {
        let (rig, device, _audio) = Rig::with_audio(PERCENT);
        let player = rig.player("one", Playback::Playing);
        let (out, paused) =
            placed_until(&device, InEar, OutOfEar, &player, Asked::Pause, HANG_LIMIT);
        assert_gap(
            "the pause after one bud left",
            out,
            paused,
            &(Duration::ZERO..=PROMPT),
        );
        assert_eq!(player.playback(), Playback::Paused);
        let (back, resumed) = placed_until(&device, InEar, InEar, &player, Asked::Play, HANG_LIMIT);
        assert_gap(
            "the resume after both buds came back",
            back,
            resumed,
            &(Duration::ZERO..=PROMPT),
        );
        assert_eq!(player.asked(), [Asked::Pause, Asked::Play]);
        player.leave();
        rig.finish();
    }

    #[test]
    fn with_both_removed_one_bud_out_pauses_nothing() {
        let (rig, device, _audio) = Rig::with_audio(PERCENT);
        prefer(&rig, "both", 1);
        let player = rig.player("one", Playback::Playing);
        place(&rig, &device, InEar, OutOfEar);
        // A pause at the first bud would have come long before the settle of the second.
        let within = SETTLE.saturating_add(HANG_LIMIT);
        let (out, paused) =
            placed_until(&device, OutOfEar, OutOfEar, &player, Asked::Pause, within);
        assert_gap(
            "the pause after both buds left",
            out,
            paused,
            &not_before(SETTLE),
        );
        assert_eq!(player.asked(), [Asked::Pause]);
        player.leave();
        rig.finish();
    }

    #[test]
    fn both_out_pauses_after_the_settle_and_one_in_resumes() {
        let (rig, device, _audio) = Rig::with_audio(PERCENT);
        prefer(&rig, "both", 1);
        let player = rig.player("one", Playback::Playing);
        let within = SETTLE.saturating_add(HANG_LIMIT);
        let (out, paused) =
            placed_until(&device, OutOfEar, OutOfEar, &player, Asked::Pause, within);
        assert_gap(
            "the pause after both buds left",
            out,
            paused,
            &not_before(SETTLE),
        );
        let (back, resumed) =
            placed_until(&device, OutOfEar, InEar, &player, Asked::Play, HANG_LIMIT);
        assert_gap(
            "the resume after one bud came back",
            back,
            resumed,
            &(Duration::ZERO..=PROMPT),
        );
        assert_eq!(player.asked(), [Asked::Pause, Asked::Play]);
        player.leave();
        rig.finish();
    }

    #[test]
    fn a_bud_back_inside_the_settle_cancels_the_pause() {
        let (rig, device, _audio) = Rig::with_audio(PERCENT);
        prefer(&rig, "both", 1);
        let player = rig.player("one", Playback::Playing);
        let out = Instant::now();
        place(&rig, &device, OutOfEar, OutOfEar);
        place(&rig, &device, InEar, OutOfEar);
        assert!(
            out.elapsed() < SETTLE,
            "the bud came back only after {:?}",
            out.elapsed()
        );
        quiet_until(&player, out + SETTLE + REAL_TIME_SLACK);
        // The same daemon still pauses once both buds stay out.
        let within = SETTLE.saturating_add(HANG_LIMIT);
        let (again, paused) =
            placed_until(&device, OutOfEar, OutOfEar, &player, Asked::Pause, within);
        assert_gap(
            "the pause after both buds stayed out",
            again,
            paused,
            &not_before(SETTLE),
        );
        player.leave();
        rig.finish();
    }

    #[test]
    fn ear_detection_off_pauses_nothing() {
        let (rig, device, _audio) = Rig::with_audio(PERCENT);
        prefer(&rig, "off", 2);
        let player = rig.player("one", Playback::Playing);
        let out = Instant::now();
        place(&rig, &device, InEar, OutOfEar);
        place(&rig, &device, OutOfEar, OutOfEar);
        quiet_until(&player, out + SETTLE + REAL_TIME_SLACK);
        assert_eq!(player.asked(), []);
        prefer(&rig, "one", 0);
        place(&rig, &device, InEar, InEar);
        placed_until(&device, InEar, OutOfEar, &player, Asked::Pause, HANG_LIMIT);
        player.leave();
        rig.finish();
    }

    #[test]
    fn a_player_already_paused_gets_no_pause_and_no_play() {
        let (rig, device, _audio) = Rig::with_audio(PERCENT);
        let playing = rig.player("playing", Playback::Playing);
        let paused = rig.player("paused", Playback::Paused);
        placed_until(&device, InEar, OutOfEar, &playing, Asked::Pause, HANG_LIMIT);
        placed_until(&device, InEar, InEar, &playing, Asked::Play, HANG_LIMIT);
        assert_eq!(paused.asked(), []);
        paused.none_for(QUIET);
        assert_eq!(paused.playback(), Playback::Paused);
        assert_eq!(paused.reads(), 1);
        playing.leave();
        paused.leave();
        rig.finish();
    }

    #[test]
    fn a_player_the_user_paused_is_never_resumed() {
        let (rig, device, _audio) = Rig::with_audio(PERCENT);
        let ours = rig.player("ours", Playback::Playing);
        let users = rig.player("users", Playback::Paused);
        placed_until(&device, InEar, OutOfEar, &ours, Asked::Pause, HANG_LIMIT);
        // Read once by the pause, so starting it now is the user's own doing.
        users.wait_reads(1, HANG_LIMIT);
        users.set(Playback::Playing);
        users.set(Playback::Paused);
        // Each change was announced with `PropertiesChanged`, so a daemon watching players saw this one play.
        placed_until(&device, InEar, InEar, &ours, Asked::Play, HANG_LIMIT);
        assert_eq!(users.asked(), []);
        users.none_for(QUIET);
        assert_eq!(users.playback(), Playback::Paused);
        ours.leave();
        users.leave();
        rig.finish();
    }

    #[test]
    fn two_players_playing_are_both_paused_and_both_resumed() {
        let (rig, device, _audio) = Rig::with_audio(PERCENT);
        let first = rig.player("first", Playback::Playing);
        let second = rig.player("second", Playback::Playing);
        placed_until(&device, InEar, OutOfEar, &first, Asked::Pause, HANG_LIMIT);
        second.wait_asked(Asked::Pause, HANG_LIMIT);
        placed_until(&device, InEar, InEar, &first, Asked::Play, HANG_LIMIT);
        second.wait_asked(Asked::Play, HANG_LIMIT);
        for player in [&first, &second] {
            assert_eq!(player.asked(), [Asked::Pause, Asked::Play]);
            assert_eq!(player.playback(), Playback::Playing);
        }
        first.leave();
        second.leave();
        rig.finish();
    }

    #[test]
    fn pods_that_are_not_the_default_sink_pause_nothing() {
        let mut rig = Rig::new();
        let audio = rig.start_audio();
        let device = rig.add_pods(Profile::default());
        let sink = sink_named(device.address);
        audio.add_sink(&sink, PERCENT);
        audio.add_sink(SPEAKERS, PERCENT);
        audio.set_default_sink(SPEAKERS);
        rig.start();
        rig.link_up(&device);
        rig.wait_filled();
        rig.wait_audio_server("up");
        let player = rig.player("one", Playback::Playing);
        place(&rig, &device, InEar, OutOfEar);
        player.none_for(QUIET);
        assert_eq!(player.asked(), []);
        // The same daemon pauses once the pods' sink is the default.
        audio.set_default_sink(&sink);
        told(&audio, &sink);
        place(&rig, &device, InEar, InEar);
        placed_until(&device, InEar, OutOfEar, &player, Asked::Pause, HANG_LIMIT);
        player.leave();
        rig.finish();
    }

    #[test]
    fn no_audio_server_pauses_nothing() {
        let mut rig = Rig::new();
        let device = rig.add_pods(Profile::default());
        rig.start();
        rig.link_up(&device);
        rig.wait_filled();
        rig.wait_logged("audio: the audio server is down: No such file or directory (os error 2)");
        let player = rig.player("one", Playback::Playing);
        place(&rig, &device, InEar, OutOfEar);
        player.none_for(QUIET);
        assert_eq!(player.asked(), []);
        // The same daemon pauses once a server holds the pods' sink as its default.
        let audio = rig.start_audio();
        let sink = sink_named(device.address);
        audio.add_sink(&sink, PERCENT);
        audio.set_default_sink(&sink);
        rig.wait_audio_server("up");
        told(&audio, &sink);
        place(&rig, &device, InEar, InEar);
        placed_until(&device, InEar, OutOfEar, &player, Asked::Pause, HANG_LIMIT);
        player.leave();
        rig.finish();
    }

    #[test]
    fn the_primary_bud_changing_is_followed_in_status() {
        let (rig, device, _audio) = Rig::with_audio(PERCENT);
        let player = rig.player("one", Playback::Playing);
        placed_until(&device, InEar, OutOfEar, &player, Asked::Pause, HANG_LIMIT);
        wait_worn(&rig, true, false);
        player.set(Playback::Playing);
        // The left bud becomes the primary, so the same report now reads it in the ear.
        device.pods.send(&left_first_battery());
        wait_worn(&rig, false, true);
        // The pods report again in the new order: the left bud, now primary, out.
        device.pods.set_ear(OutOfEar, InEar);
        wait_worn(&rig, true, false);
        player.none_for(QUIET);
        assert_eq!(player.asked(), [Asked::Pause]);
        player.leave();
        rig.finish();
    }

    #[test]
    fn flapping_buds_and_a_preference_change_leave_no_stale_pause() {
        let (rig, device, _audio) = Rig::with_audio(PERCENT);
        prefer(&rig, "both", 1);
        let player = rig.player("one", Playback::Playing);
        let start = Instant::now();
        device.pods.set_ear(OutOfEar, OutOfEar);
        pause((start + FLAP).saturating_duration_since(Instant::now()));
        device.pods.set_ear(InEar, InEar);
        pause((start + 2 * FLAP).saturating_duration_since(Instant::now()));
        let last_out = Instant::now();
        device.pods.set_ear(OutOfEar, OutOfEar);
        // Past the first out's settle, so a timer the flap left running has paused by now.
        pause((start + SETTLE + QUIET).saturating_duration_since(Instant::now()));
        assert_eq!(player.asked(), []);
        prefer(&rig, "off", 2);
        assert!(
            last_out.elapsed() < SETTLE,
            "the preference changed only after {:?}",
            last_out.elapsed()
        );
        quiet_until(&player, last_out + SETTLE + REAL_TIME_SLACK);
        assert_eq!(player.asked(), []);
        prefer(&rig, "one", 0);
        place(&rig, &device, InEar, InEar);
        placed_until(&device, InEar, OutOfEar, &player, Asked::Pause, HANG_LIMIT);
        player.leave();
        rig.finish();
    }

    #[test]
    fn a_pending_pause_dies_with_the_link() {
        let (rig, device, _audio) = Rig::with_audio(PERCENT);
        prefer(&rig, "both", 1);
        let player = rig.player("one", Playback::Playing);
        let out = Instant::now();
        place(&rig, &device, OutOfEar, OutOfEar);
        // A user disconnect ends the link at once, where a lost device is held for a grace.
        rig.disconnect();
        quiet_until(&player, out + SETTLE + REAL_TIME_SLACK);
        // Linked again, the same daemon pauses, and what it paused is forgotten with the next loss.
        rig.relink(&device);
        rig.wait_audio_server("up");
        place(&rig, &device, InEar, InEar);
        let within = SETTLE.saturating_add(HANG_LIMIT);
        placed_until(&device, OutOfEar, OutOfEar, &player, Asked::Pause, within);
        rig.disconnect();
        rig.relink(&device);
        rig.wait_audio_server("up");
        // From both out, so one bud back is the resume's own trigger in this mode.
        place(&rig, &device, OutOfEar, OutOfEar);
        place(&rig, &device, InEar, OutOfEar);
        player.none_for(QUIET);
        assert_eq!(player.asked(), [Asked::Pause]);
        player.leave();
        rig.finish();
    }

    #[test]
    fn a_hung_player_delays_no_other_player_and_status_still_answers() {
        let (rig, device, _audio) = Rig::with_audio(PERCENT);
        let first = rig.player("first", Playback::Playing);
        let second = rig.player("second", Playback::Playing);
        // Each hangs in turn and the bus lists the same names in the same order, so a serial daemon meets the hung one first once.
        for (hung, fine) in [(&first, &second), (&second, &first)] {
            hung.hold_replies(true);
            let reads = hung.reads();
            let asked = hung.asked();
            let (out, paused) =
                placed_until(&device, InEar, OutOfEar, fine, Asked::Pause, HANG_LIMIT);
            assert_gap(
                "the other player's pause while one hangs",
                out,
                paused,
                &(Duration::ZERO..=CALL_TIMEOUT),
            );
            let status_asked = Instant::now();
            let status = rig.status();
            assert_gap(
                "status while a player hangs",
                status_asked,
                Instant::now(),
                &(Duration::ZERO..=ANSWERED_WITHIN),
            );
            assert!(connected(&status), "{status}");
            hung.wait_reads(reads.saturating_add(1), HANG_LIMIT);
            // The hung player never took the pause, so the resume leaves it alone.
            placed_until(&device, InEar, InEar, fine, Asked::Play, HANG_LIMIT);
            hung.none_for(QUIET);
            // Answered long after the daemon stopped waiting, which changes nothing.
            hung.hold_replies(false);
            hung.none_for(QUIET);
            assert_eq!(hung.asked(), asked);
            assert_eq!(hung.playback(), Playback::Playing);
        }
        first.leave();
        second.leave();
        rig.finish();
    }

    #[test]
    fn a_pause_answered_after_the_timeout_is_still_resumed() {
        let (mut rig, device, _audio) = Rig::with_audio(PERCENT);
        let slow = rig.player("slow", Playback::Playing);
        slow.hold_asked(true);
        placed_until(&device, InEar, OutOfEar, &slow, Asked::Pause, HANG_LIMIT);
        pause(CALL_TIMEOUT.saturating_add(QUIET));
        // Asked while the pause is still unanswered, so the daemon stopped waiting for that answer.
        placed_until(&device, InEar, InEar, &slow, Asked::Play, HANG_LIMIT);
        let timeout_ms = CALL_TIMEOUT.as_millis();
        rig.wait_logged(&format!(
            "media: 0 of 1 players refused the resume and 1 did not answer within {timeout_ms} ms"
        ));
        // The pause lands late, then the resume.
        slow.hold_asked(false);
        assert_eq!(slow.asked(), [Asked::Pause, Asked::Play]);
        assert_eq!(slow.playback(), Playback::Playing);
        slow.leave();
        rig.finish();
    }

    #[test]
    fn more_players_than_the_bound_are_cut_off() {
        let (mut rig, device, _audio) = Rig::with_audio(PERCENT);
        let players: Vec<FakePlayer> = (0..PLAYERS_ON_A_BUSY_BUS)
            .map(|index| rig.player(&format!("busy{index}"), Playback::Playing))
            .collect();
        let count = |call: Asked| -> usize {
            let each = players.iter().map(|player| {
                player
                    .asked()
                    .iter()
                    .filter(|asked| **asked == call)
                    .count()
            });
            each.sum()
        };
        device.pods.set_ear(InEar, OutOfEar);
        let line = format!(
            "media: {PLAYERS_ON_A_BUSY_BUS} players on the session bus; only the first {PLAYERS_MAX} are asked"
        );
        rig.wait_logged(&line);
        wait_for("the bound's worth of pauses", || {
            (count(Asked::Pause) == PLAYERS_MAX).then_some(())
        });
        device.pods.set_ear(InEar, InEar);
        wait_for("the same players resumed", || {
            (count(Asked::Play) == PLAYERS_MAX).then_some(())
        });
        for player in &players {
            let asked = player.asked();
            assert!(
                asked.is_empty() || asked == [Asked::Pause, Asked::Play],
                "{}: {asked:?}",
                player.bus_name()
            );
        }
        let reads: usize = players.iter().map(FakePlayer::reads).sum();
        assert_eq!(reads, PLAYERS_MAX);
        assert_eq!(count(Asked::Pause), PLAYERS_MAX);
        assert_eq!(rig.daemon().log().matches(&line).count(), 1);
        for player in players {
            player.leave();
        }
        rig.finish();
    }

    #[test]
    fn a_player_that_left_before_the_resume_is_skipped() {
        let (mut rig, device, _audio) = Rig::with_audio(PERCENT);
        let staying = rig.player("staying", Playback::Playing);
        let leaving = rig.player("leaving", Playback::Playing);
        placed_until(&device, InEar, OutOfEar, &staying, Asked::Pause, HANG_LIMIT);
        leaving.wait_asked(Asked::Pause, HANG_LIMIT);
        leaving.leave();
        let (back, resumed) =
            placed_until(&device, InEar, InEar, &staying, Asked::Play, HANG_LIMIT);
        assert_gap(
            "the resume with one player gone",
            back,
            resumed,
            &(Duration::ZERO..=PROMPT),
        );
        assert_eq!(staying.asked(), [Asked::Pause, Asked::Play]);
        // Past the call deadline, so a resume refused or unanswered would have logged by now.
        pause(CALL_TIMEOUT.saturating_add(QUIET));
        let log = rig.daemon().log();
        assert!(!log.contains("refused the resume"), "{log}");
        staying.leave();
        rig.finish();
    }

    #[test]
    fn a_pause_still_pending_when_the_link_ends_is_never_sent() {
        let (rig, device, _audio) = Rig::with_audio(PERCENT);
        let player = rig.player("slow", Playback::Playing);
        player.hold_replies(true);
        device.pods.set_ear(InEar, OutOfEar);
        player.wait_reads(1, HANG_LIMIT);
        let read = Instant::now();
        rig.disconnect();
        // Inside the daemon's wait for the status read, or the answer below would reach nobody either way.
        assert_gap(
            "the disconnect while the status read is pending",
            read,
            Instant::now(),
            &(Duration::ZERO..=CALL_TIMEOUT),
        );
        player.hold_replies(false);
        player.none_for(QUIET);
        assert_eq!(player.asked(), []);
        assert_eq!(player.playback(), Playback::Playing);
        player.leave();
        rig.finish();
    }

    #[test]
    fn a_player_reopened_under_the_same_name_is_not_started() {
        let (rig, device, _audio) = Rig::with_audio(PERCENT);
        let staying = rig.player("staying", Playback::Playing);
        let closed = rig.player("reopened", Playback::Playing);
        placed_until(&device, InEar, OutOfEar, &staying, Asked::Pause, HANG_LIMIT);
        closed.wait_asked(Asked::Pause, HANG_LIMIT);
        closed.leave();
        let reopened = rig.player("reopened", Playback::Paused);
        // The staying player's resume shows the return was served, and both resumes go out together.
        placed_until(&device, InEar, InEar, &staying, Asked::Play, HANG_LIMIT);
        reopened.none_for(QUIET);
        assert_eq!(reopened.asked(), []);
        assert_eq!(reopened.playback(), Playback::Paused);
        staying.leave();
        reopened.leave();
        rig.finish();
    }

    #[test]
    fn a_pause_still_pending_when_the_channel_closes_is_never_sent() {
        let (rig, device, _audio) = Rig::with_audio(PERCENT);
        let player = rig.player("slow", Playback::Playing);
        player.hold_replies(true);
        let out = Instant::now();
        device.pods.set_ear(InEar, OutOfEar);
        player.wait_reads(1, HANG_LIMIT);
        rig.close_channel(&device);
        // Inside the daemon's wait for the status read, or the answer below would reach nobody either way.
        assert_gap(
            "the close while the status read is pending",
            out,
            Instant::now(),
            &(Duration::ZERO..=CALL_TIMEOUT),
        );
        player.hold_replies(false);
        player.none_for(QUIET);
        assert_eq!(player.asked(), []);
        assert_eq!(player.playback(), Playback::Playing);
        player.leave();
        rig.finish();
    }

    #[test]
    fn a_settle_pending_when_the_channel_closes_pauses_nothing() {
        let (rig, device, _audio) = Rig::with_audio(PERCENT);
        prefer(&rig, "both", 1);
        let player = rig.player("one", Playback::Playing);
        let out = Instant::now();
        place(&rig, &device, OutOfEar, OutOfEar);
        rig.close_channel(&device);
        assert_gap(
            "the close inside the settle",
            out,
            Instant::now(),
            &(Duration::ZERO..=SETTLE),
        );
        // The settle began before `place` returned, so it ends inside this window.
        quiet_until(&player, Instant::now() + SETTLE + REAL_TIME_SLACK);
        assert_eq!(player.asked(), []);
        player.leave();
        rig.finish();
    }

    #[test]
    fn a_channel_closing_with_the_buds_in_keeps_them_worn_and_pauses_nothing() {
        let (rig, device, _audio) = Rig::with_audio(PERCENT);
        let player = rig.player("one", Playback::Playing);
        place(&rig, &device, InEar, InEar);
        rig.close_channel(&device);
        player.none_for(QUIET);
        assert_eq!(player.asked(), []);
        let status = rig.status();
        assert_eq!(status["right"]["in_ear"], true, "{status}");
        assert_eq!(status["left"]["in_ear"], true, "{status}");
        player.leave();
        rig.finish();
    }

    #[test]
    fn a_player_paused_before_the_channel_closed_is_played_when_it_reopens_with_the_buds_back() {
        let (rig, device, _audio) = Rig::with_audio(PERCENT);
        let player = rig.player("one", Playback::Playing);
        placed_until(&device, InEar, OutOfEar, &player, Asked::Pause, HANG_LIMIT);
        // Time for the pause's report to reach the policy before the close drops work still in flight.
        pause(QUIET);
        rig.close_channel(&device);
        rig.accept(&device);
        rig.wait_filled();
        rig.wait_audio_server("up");
        placed_until(&device, InEar, InEar, &player, Asked::Play, HANG_LIMIT);
        assert_eq!(player.asked(), [Asked::Pause, Asked::Play]);
        player.leave();
        rig.finish();
    }

    #[test]
    fn a_resume_queued_when_the_channel_closes_still_plays_and_the_pause_ahead_of_it_does_not() {
        resume_outlives_the_end("the close", Rig::close_channel);
    }

    #[test]
    fn a_resume_queued_when_the_link_ends_still_plays_and_the_pause_ahead_of_it_does_not() {
        resume_outlives_the_end("the disconnect", |rig, _| rig.disconnect());
    }

    /// Pauses `first` by a bud leaving, holds a second pause on a slow player, returns both buds so the resume waits behind it, then ends the link with `end`.
    fn resume_outlives_the_end(what: &str, end: impl FnOnce(&Rig, &Device)) {
        let (rig, device, _audio) = Rig::with_audio(PERCENT);
        let first = rig.player("first", Playback::Playing);
        placed_until(&device, InEar, OutOfEar, &first, Asked::Pause, HANG_LIMIT);
        // Time for the pause's report to reach the policy, so the return below asks this player.
        pause(QUIET);
        let slow = rig.player("slow", Playback::Playing);
        slow.hold_replies(true);
        let out = Instant::now();
        device.pods.set_ear(OutOfEar, OutOfEar);
        slow.wait_reads(1, HANG_LIMIT);
        place(&rig, &device, InEar, InEar);
        end(&rig, &device);
        // Inside the second pause's wait for the slow read, so the resume is still queued behind it.
        assert_gap(
            &format!("{what} while the second pause is pending"),
            out,
            Instant::now(),
            &(Duration::ZERO..=CALL_TIMEOUT),
        );
        first.wait_asked(Asked::Play, HANG_LIMIT);
        slow.hold_replies(false);
        slow.none_for(QUIET);
        assert_eq!(slow.asked(), []);
        assert_eq!(first.asked(), [Asked::Pause, Asked::Play]);
        slow.leave();
        first.leave();
        rig.finish();
    }
}
