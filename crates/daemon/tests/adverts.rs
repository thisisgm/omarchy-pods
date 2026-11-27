//! The pods' proximity adverts through the built daemon: the one advert monitor it registers with the fake BlueZ, and what an advert from its own pods sets on every surface.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
#[path = "adverts/pairs.rs"]
mod pairs;

#[cfg(test)]
#[path = "adverts/wear.rs"]
mod wear;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use pods_fake::Profile;
    use pods_samples::advert::{HEADER_LEN, SEALED_LEN, advert_of};
    use pods_trap::Answer;
    use rustix::io::Errno;
    use serde_json::{Value, json};
    use zbus::OwnedVariant;

    use super::support::advert::{CHARGING, HEADER, PAYLOAD, RANDOM, keyed, keys, ours};
    use super::support::bluez::monitor::{LATE_BY, Registered, manager_call, registration};
    use super::support::bluez::{Call, DeviceMethod};
    use super::support::bus::{BUS_NAME, INTERFACE, Listener, OBJECT_PATH, changes_rule};
    use super::support::omarchy::{RECORDED_WITHIN, low_battery_notice};
    use super::support::players::PROPERTIES;
    use super::support::rig::{Rig, assert_gap};
    use super::support::sleep::logged;
    use super::support::{HANG_LIMIT, Proto1, do_request, outcome, pause, wait_for};

    /// The same pods with the case at 10 percent and not charging.
    pub(super) const LOW_CASE_HEADER: [u8; HEADER_LEN] = [
        0x07, 0x19, 0x01, 0x27, 0x20, 0x62, 0x88, 0x01, 0x00, 0x00, 0x04,
    ];
    /// The primary byte of 75 percent and charging.
    const PRIMARY_75_CHARGING: u8 = 0xcb;
    const PRIMARY_AT: usize = 1;
    const SECONDARY_AT: usize = 2;
    const CASE_PAYLOAD_AT: usize = 3;
    /// The decrypted payload's marker of a level it does not know.
    const UNKNOWN_LEVEL: u8 = 0x7f;
    /// One past the highest level a battery can read.
    const PAST_FULL: u8 = 101;
    /// The decrypted case byte of 50 percent and not charging.
    const CASE_50: u8 = 0x32;
    /// The header byte of the case's tenths and charging flag.
    pub(super) const CASE_AT: usize = 7;
    /// The case charging at the unknown 15 tenths, so the case comes from the decrypted payload instead.
    const CASE_UNKNOWN: u8 = 0x4f;
    const OTHER_IDENTITY: [u8; 16] = [0x11; 16];
    /// The most an advert may take to reach the status document, polling included.
    const SEEN_WITHIN: Duration = Duration::from_secs(1);
    /// Long enough for a monitor call the daemon wrongly makes to reach the fake BlueZ.
    pub(super) const QUIET: Duration = Duration::from_millis(500);
    /// The adapters offering monitors in the two adapter case.
    const ADAPTERS: usize = 2;
    /// The monitor calls of one try there: an unregistration and a registration on each adapter.
    const TRY_CALLS: usize = 2 * ADAPTERS;
    /// The daemon's tries at the monitor before it reads unavailable.
    const MONITOR_TRIES: usize = 5;
    /// The daemon's wait between a refused try and the next.
    const MONITOR_RETRY: Duration = Duration::from_secs(1);
    /// Failed dials watched after the channel closes: 750 ms, then 1 s and 2 s up the held ladder.
    const RETRIES_SEEN: usize = 3;
    /// Past the five tries a second apart, so a sixth would have been made.
    const PAST_THE_TRIES: Duration = Duration::from_millis(6500);
    /// One byte short of the advert the daemon reads.
    const SHORT_LEN: usize = HEADER_LEN + SEALED_LEN - 1;
    /// A first byte other than the proximity type, which the monitor's pattern leaves out.
    const WRONG_TYPE: u8 = 0x10;
    /// Bytes past the advert the daemon reads, which it ignores.
    const TRAILING: [u8; 4] = [0xee; 4];
    /// Percent between one timed advert and the next.
    const LEVEL_STEP: u8 = 10;
    /// The random part of the flood's addresses, ahead of each address's own byte.
    const FLOOD_RANDOM: [u8; 2] = [0x40, 0x01];
    /// The adverts of the flood before the status is asked, and the addresses they come from.
    const FLOOD: usize = 2000;
    const FLOOD_ADDRESSES: u8 = 40;
    /// Over five times the most growth measured over the flood (192 KiB), under the 1288 KiB that 512 bytes kept per advert grew it by.
    const RSS_GROWTH_MAX_KIB: u64 = 1024;
    /// The adverts timed one by one, for the range of an advert to a surface.
    const TIMED: u8 = 5;
    const APPLE_PATTERN: (u8, u8, [u8; 3]) = (0, 0xff, [0x4c, 0x00, 0x07]);

    /// Started, and the monitor read by the fake BlueZ.
    fn listening() -> Rig {
        let mut rig = keyed();
        rig.start();
        wait_intake(&rig, "listening");
        rig
    }

    pub(super) fn wait_intake(rig: &Rig, word: &str) -> Value {
        rig.wait_status(&format!("advert_intake {word}"), |status| {
            status["advert_intake"] == word
        })
    }

    fn apple_monitor() -> Registered {
        let (start, ad_type, content) = APPLE_PATTERN;
        Registered {
            kind: "or_patterns".to_owned(),
            patterns: vec![(start, ad_type, content.to_vec())],
        }
    }

    /// What an advert of `HEADER` and `PAYLOAD` from our pods sets.
    pub(super) fn filled() -> Value {
        json!({
            "advert_intake": "listening",
            "case": {"available": true, "charging": true, "level": 70},
            "lid_state": 0,
            "left": {"available": true, "charging": true, "in_ear": false, "level": 80},
            "model_name": "AirPods Pro 3",
            "right": {"available": true, "charging": false, "in_ear": true, "level": 60},
        })
    }

    /// The keys an advert sets, picked out of a whole status.
    pub(super) fn picked(status: &Value) -> Value {
        let keys = [
            "advert_intake",
            "case",
            "lid_state",
            "left",
            "model_name",
            "right",
        ];
        Value::Object(
            keys.iter()
                .map(|key| ((*key).to_owned(), status[*key].clone()))
                .collect(),
        )
    }

    fn json_of(bytes: &[u8]) -> Value {
        serde_json::from_slice(bytes).unwrap_or_else(|error| {
            panic!("{error}: {}", String::from_utf8_lossy(bytes));
        })
    }

    /// The daemon's interface property, once the daemon owns its name.
    fn property(listener: &Listener, name: &str) -> String {
        wait_for(&format!("the {name} property"), || {
            let reply = listener.call(BUS_NAME, OBJECT_PATH, PROPERTIES, "Get", &(INTERFACE, name));
            let variant: OwnedVariant = reply.ok()?.body().deserialize().ok()?;
            String::try_from(variant).ok()
        })
    }

    /// The four surfaces in turn: `librepods status`, the state file, the D-Bus interface and `librepods ctl status`.
    fn surfaces(rig: &Rig) -> [Value; 4] {
        let status = picked(&rig.cli_status());
        let state_file = wait_for("the state file showing the status", || {
            let text = fs::read_to_string(rig.session().state_file()).ok()?;
            let state = picked(&serde_json::from_str::<Value>(&text).ok()?);
            (state == status).then_some(state)
        });
        let listener = Listener::subscribe(rig.session_bus(), &changes_rule());
        let mut over_dbus = picked(&json_of(property(&listener, "StatusJson").as_bytes()));
        over_dbus["advert_intake_property"] = Value::from(property(&listener, "AdvertIntake"));
        let ctl = rig.client(&["ctl", "status"]);
        assert!(ctl.status.success(), "ctl status: {ctl:?}");
        [status, state_file, over_dbus, picked(&json_of(&ctl.stdout))]
    }

    /// `VmRSS` of the process in KiB; sample input line `VmRSS:\t   10240 kB`.
    fn resident_kib(pid: u32) -> u64 {
        let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
        let line = status
            .lines()
            .find(|line| line.starts_with("VmRSS:"))
            .unwrap();
        let kib = line
            .trim_start_matches("VmRSS:")
            .trim()
            .trim_end_matches("kB");
        kib.trim().parse().unwrap()
    }

    #[test]
    fn the_monitor_is_registered_with_the_apple_proximity_pattern() {
        let rig = listening();
        assert_eq!(rig.bluez().monitors(), [apple_monitor()]);
        assert_eq!(rig.bluez().calls(), registration(rig.adapter()));
        rig.finish();
    }

    #[test]
    #[expect(
        clippy::print_stderr,
        reason = "the range an advert took to the status goes into the test's output"
    )]
    fn an_advert_from_our_pods_fills_battery_lid_and_ear_while_the_link_is_down() {
        let mut rig = keyed();
        rig.add_pods(Profile::default());
        rig.start();
        wait_intake(&rig, "listening");
        let sent = Instant::now();
        let advertiser = rig
            .bluez()
            .advertise(&ours(HEADER, PAYLOAD).address, &ours(HEADER, PAYLOAD).data);
        rig.wait_status("the advert in the status", |status| {
            picked(status) == filled()
        });
        assert_gap(
            "an advert to the status",
            sent,
            Instant::now(),
            &(Duration::ZERO..=SEEN_WITHIN),
        );
        let [status, state_file, mut over_dbus, ctl] = surfaces(&rig);
        assert_eq!(status, filled());
        assert_eq!(state_file, filled());
        assert_eq!(over_dbus["advert_intake_property"], "listening");
        over_dbus
            .as_object_mut()
            .unwrap()
            .remove("advert_intake_property");
        assert_eq!(over_dbus, filled());
        assert_eq!(ctl, filled());
        let mut gaps = Vec::new();
        for level in 1..=TIMED {
            let mut payload = PAYLOAD;
            payload[PRIMARY_AT] = CHARGING | level.saturating_mul(LEVEL_STEP);
            let advert = ours(HEADER, payload);
            let sent = Instant::now();
            advertiser.update(&advert.data);
            let wanted = u64::from(level.saturating_mul(LEVEL_STEP));
            rig.wait_status("the next advert", |status| {
                status["left"]["level"] == wanted
            });
            gaps.push(sent.elapsed());
        }
        gaps.sort();
        eprintln!(
            "an advert to the status, {TIMED} adverts: {:?} to {:?}",
            gaps.first().unwrap(),
            gaps.last().unwrap()
        );
        assert!(gaps.iter().all(|gap| *gap <= SEEN_WITHIN), "{gaps:?}");
        assert!(!rig.status()["connected"].as_bool().unwrap());
        rig.no_dial_for(QUIET);
        assert_eq!(rig.bluez().calls(), registration(rig.adapter()));
        rig.finish();
    }

    #[test]
    fn an_advert_from_other_pods_changes_nothing() {
        let rig = listening();
        let before = picked(&rig.status());
        let theirs = advert_of(HEADER, PAYLOAD, &keys(OTHER_IDENTITY), RANDOM);
        let _theirs = rig.bluez().advertise(&theirs.address, &theirs.data);
        rig.holds_for(SEEN_WITHIN, "the status as it was", |status| {
            picked(status) == before
        });
        let ours = ours(HEADER, PAYLOAD);
        let _ours = rig.bluez().advertise(&ours.address, &ours.data);
        rig.wait_status("our advert after theirs", |status| {
            picked(status) == filled()
        });
        assert_eq!(rig.bluez().calls(), registration(rig.adapter()));
        rig.finish();
    }

    #[test]
    fn an_advert_with_unknown_levels_keeps_the_last_reading() {
        let rig = listening();
        let first = ours(HEADER, PAYLOAD);
        let advertiser = rig.bluez().advertise(&first.address, &first.data);
        rig.wait_status("the first advert", |status| picked(status) == filled());
        let mut header = HEADER;
        header[CASE_AT] = CASE_UNKNOWN;
        let mut payload = PAYLOAD;
        payload[PRIMARY_AT] = UNKNOWN_LEVEL;
        payload[SECONDARY_AT] = PAST_FULL;
        payload[CASE_PAYLOAD_AT] = CASE_50;
        advertiser.update(&ours(header, payload).data);
        let mut wanted = filled();
        wanted["case"] = json!({"available": true, "charging": false, "level": 50});
        rig.wait_status("the second advert's case", |status| {
            status["case"] == wanted["case"]
        });
        let [status, state_file, ..] = surfaces(&rig);
        assert_eq!(status, wanted);
        assert_eq!(state_file, wanted);
        rig.finish();
    }

    #[test]
    fn without_keys_nothing_is_registered_and_intake_reads_paused() {
        let mut rig = Rig::new();
        rig.bluez().add_monitor_manager(rig.adapter());
        rig.start();
        let advert = ours(HEADER, PAYLOAD);
        let _advertiser = rig.bluez().advertise(&advert.address, &advert.data);
        rig.holds_for(QUIET, "the intake paused and nothing filled", |status| {
            status["advert_intake"] == "paused" && status["case"]["available"] == false
        });
        assert_eq!(rig.bluez().monitors(), []);
        assert_eq!(rig.bluez().calls(), []);
        rig.finish();
    }

    #[test]
    fn the_monitor_is_dropped_while_the_link_is_up_and_back_after_it_is_lost() {
        let mut rig = keyed();
        let device = rig.add_pods(Profile::default());
        rig.start();
        wait_intake(&rig, "listening");
        rig.link_up(&device);
        rig.wait_filled();
        rig.bluez().wait_monitors(0, HANG_LIMIT);
        wait_intake(&rig, "paused");
        rig.link_down(&device);
        rig.bluez().wait_monitors(1, HANG_LIMIT);
        wait_intake(&rig, "listening");
        let mut wanted = registration(rig.adapter());
        wanted.push(manager_call(rig.adapter(), DeviceMethod::UnregisterMonitor));
        wanted.extend(registration(rig.adapter()));
        assert_eq!(rig.bluez().calls(), wanted);
        rig.finish();
    }

    #[test]
    fn a_closed_channel_with_the_pods_still_connected_reads_adverts_until_it_reopens() {
        let mut rig = keyed();
        let device = rig.add_pods(Profile::default());
        rig.session().store_sample_keys(Some(device.address));
        rig.start();
        wait_intake(&rig, "listening");
        rig.link_up(&device);
        rig.wait_filled();
        rig.bluez().wait_monitors(0, HANG_LIMIT);
        wait_intake(&rig, "paused");
        let refused = Answer::Refuse {
            after: Duration::ZERO,
            errno: Errno::CONNREFUSED,
        };
        rig.answer(&device, refused);
        device.pods.hang_up();
        rig.bluez().wait_monitors(1, HANG_LIMIT);
        wait_intake(&rig, "listening");
        assert_ne!(picked(&rig.status()), filled(), "filled before the advert");
        let advert = ours(HEADER, PAYLOAD);
        let _advertiser = rig.bluez().advertise(&advert.address, &advert.data);
        rig.wait_status("the advert in the status", |status| {
            picked(status) == filled()
        });
        let [status, state_file, mut over_dbus, ctl] = surfaces(&rig);
        assert_eq!(status, filled());
        assert_eq!(state_file, filled());
        assert_eq!(over_dbus["advert_intake_property"], "listening");
        over_dbus
            .as_object_mut()
            .unwrap()
            .remove("advert_intake_property");
        assert_eq!(over_dbus, filled());
        assert_eq!(ctl, filled());
        assert_eq!(rig.status()["connected"], false);
        rig.wait_dials(RETRIES_SEEN.saturating_add(1), HANG_LIMIT);
        let mut wanted = registration(rig.adapter());
        wanted.push(manager_call(rig.adapter(), DeviceMethod::UnregisterMonitor));
        wanted.extend(registration(rig.adapter()));
        assert_eq!(
            rig.bluez().calls(),
            wanted,
            "one registration across the failed retries"
        );
        assert_eq!(rig.status()["advert_intake"], "listening");
        rig.accept(&device);
        rig.wait_filled();
        rig.bluez().wait_monitors(0, HANG_LIMIT);
        wait_intake(&rig, "paused");
        wanted.push(manager_call(rig.adapter(), DeviceMethod::UnregisterMonitor));
        assert_eq!(rig.bluez().calls(), wanted);
        rig.finish();
    }

    #[test]
    fn an_advert_changed_as_the_first_read_is_answered_is_the_one_kept() {
        let rig = listening();
        let first = ours(HEADER, PAYLOAD);
        let mut payload = PAYLOAD;
        payload[PRIMARY_AT] = PRIMARY_75_CHARGING;
        let newer = ours(HEADER, payload);
        let _advertiser =
            rig.bluez()
                .advertise_changing_at_read(&first.address, &first.data, &newer.data);
        rig.wait_status("the newer advert", |status| status["left"]["level"] == 75);
        rig.holds_for(QUIET, "the newer advert kept", |status| {
            status["left"]["level"] == 75
        });
        assert_eq!(rig.bluez().calls(), registration(rig.adapter()));
        rig.finish();
    }

    #[test]
    fn a_low_case_in_an_advert_sends_one_notice() {
        let rig = listening();
        let low = ours(LOW_CASE_HEADER, PAYLOAD);
        let advertiser = rig.bluez().advertise(&low.address, &low.data);
        let calls = rig.omarchy().wait_calls(1, HANG_LIMIT);
        assert_eq!(calls, [low_battery_notice("Case", 10)]);
        let mut payload = PAYLOAD;
        payload[PRIMARY_AT] = PRIMARY_75_CHARGING;
        advertiser.update(&ours(LOW_CASE_HEADER, payload).data);
        rig.wait_status("the next advert", |status| status["left"]["level"] == 75);
        rig.omarchy().none_for(RECORDED_WITHIN);
        assert_eq!(rig.bluez().calls(), registration(rig.adapter()));
        rig.finish();
    }

    #[test]
    fn discovery_is_never_started_and_powered_is_never_written() {
        let rig = listening();
        let advert = ours(HEADER, PAYLOAD);
        let advertiser = rig.bluez().advertise(&advert.address, &advert.data);
        rig.wait_status("the advert in the status", |status| {
            picked(status) == filled()
        });
        rig.bluez().power(rig.adapter(), false);
        rig.bluez().power(rig.adapter(), true);
        let wanted = [registration(rig.adapter()), registration(rig.adapter())].concat();
        wait_for("a registration after the power cycle", || {
            (rig.bluez().calls() == wanted).then_some(())
        });
        // BlueZ sends a monitor no advert until it has read and activated it.
        rig.bluez().wait_monitors(1, HANG_LIMIT);
        advertiser.update(&ours(LOW_CASE_HEADER, PAYLOAD).data);
        rig.wait_status("an advert after the power cycle", |status| {
            status["case"]["level"] == 10
        });
        pause(QUIET);
        assert_eq!(rig.bluez().calls(), wanted);
        rig.finish();
    }

    #[test]
    fn an_adapter_on_another_path_works() {
        let mut rig = Rig::new();
        rig.session().store_sample_keys(None);
        let other = rig.bluez().add_adapter_at("hci7");
        rig.bluez().add_monitor_manager(&other);
        rig.start();
        wait_intake(&rig, "listening");
        let advert = ours(HEADER, PAYLOAD);
        let _advertiser = rig.bluez().advertise(&advert.address, &advert.data);
        rig.wait_status("the advert through hci7", |status| {
            picked(status) == filled()
        });
        assert_eq!(rig.bluez().calls(), registration(&other));
        rig.finish();
    }

    #[test]
    fn a_bluetoothd_gone_reads_unavailable_until_it_returns() {
        let rig = listening();
        rig.bluez().drop_name();
        wait_intake(&rig, "unavailable");
        rig.holds_for(
            SEEN_WITHIN,
            "unavailable while bluetoothd is gone",
            |status| status["advert_intake"] == "unavailable",
        );
        rig.bluez().take_name();
        wait_intake(&rig, "listening");
        let mut wanted = registration(rig.adapter());
        wanted.extend(registration(rig.adapter()));
        assert_eq!(
            rig.bluez().calls(),
            wanted,
            "registered once at start and once on the return"
        );
        rig.finish();
    }

    #[test]
    fn a_release_right_after_the_activation_registers_again() {
        let mut rig = keyed();
        rig.bluez().release_at_activation();
        rig.start();
        rig.bluez().wait_monitors(1, HANG_LIMIT);
        let mut wanted = registration(rig.adapter());
        wanted.extend(registration(rig.adapter()));
        assert_eq!(rig.bluez().calls(), wanted);
        wait_intake(&rig, "listening");
        rig.finish();
    }

    #[test]
    fn a_release_from_a_slow_unregistration_on_a_second_adapter_is_not_a_drop() {
        let mut rig = keyed();
        let second = rig.bluez().add_adapter();
        rig.bluez().add_monitor_manager(&second);
        rig.start();
        let registered = |calls: usize| {
            wait_for(&format!("{calls} monitor calls and both monitors"), || {
                let made = rig.bluez().calls().len() >= calls;
                (made && rig.bluez().monitors().len() == ADAPTERS).then_some(())
            });
        };
        registered(TRY_CALLS);
        rig.bluez().power(rig.adapter(), false);
        rig.bluez().answer_unregister_late(&second);
        rig.bluez().power(rig.adapter(), true);
        registered(2 * TRY_CALLS);
        pause(QUIET);
        let calls = rig.bluez().calls();
        assert_eq!(calls.len(), 2 * TRY_CALLS, "{calls:?}");
        let order = [
            DeviceMethod::UnregisterMonitor,
            DeviceMethod::RegisterMonitor,
        ];
        for one_try in calls.chunks(TRY_CALLS) {
            for (both, method) in one_try.chunks(ADAPTERS).zip(order.clone()) {
                for adapter in [rig.adapter(), &second] {
                    let call = manager_call(adapter, method.clone());
                    assert!(both.contains(&call), "{call:?} missing in {calls:?}");
                }
            }
        }
        let unregistered = rig.bluez().call_times(&DeviceMethod::UnregisterMonitor)[ADAPTERS];
        let registered_at = rig.bluez().call_times(&DeviceMethod::RegisterMonitor)[ADAPTERS];
        assert_gap(
            "the try's first unregistration to its first registration",
            unregistered,
            registered_at,
            &(LATE_BY..=LATE_BY.saturating_add(SEEN_WITHIN)),
        );
        wait_intake(&rig, "listening");
        rig.finish();
    }

    #[test]
    fn no_manager_interface_reads_unavailable() {
        let mut rig = Rig::new();
        rig.session().store_sample_keys(None);
        rig.start();
        wait_intake(&rig, "unavailable");
        pause(QUIET);
        assert_eq!(rig.bluez().calls(), []);
        rig.finish();
    }

    #[test]
    fn five_refusals_read_unavailable_and_a_sixth_is_not_tried() {
        let mut rig = keyed();
        rig.bluez().refuse_monitors(true);
        rig.start();
        wait_intake(&rig, "unavailable");
        let unavailable_seen = Instant::now();
        pause(PAST_THE_TRIES);
        let tries = (0..MONITOR_TRIES).flat_map(|_| registration(rig.adapter()));
        let wanted: Vec<Call> = tries.collect();
        assert_eq!(rig.bluez().calls(), wanted);
        let spacing = MONITOR_RETRY..=MONITOR_RETRY.saturating_add(SEEN_WITHIN);
        let register_times = rig.bluez().call_times(&DeviceMethod::RegisterMonitor);
        assert_eq!(register_times.len(), MONITOR_TRIES, "a time for each try");
        for (earlier, later) in register_times.iter().zip(register_times.iter().skip(1)) {
            assert_gap("one try to the next", *earlier, *later, &spacing);
        }
        let fifth_try = register_times.last().copied().unwrap();
        let after_the_fifth = Duration::ZERO..=SEEN_WITHIN;
        assert_gap(
            "the fifth try to unavailable",
            fifth_try,
            unavailable_seen,
            &after_the_fifth,
        );
        assert_eq!(rig.status()["advert_intake"], "unavailable");
        let adapter = &rig.adapter().0;
        let refused =
            format!("bluez: {adapter} refused the advert monitor: org.bluez.Error.Failed");
        assert_eq!(logged(&mut rig, &refused), 1);
        rig.finish();
    }

    #[test]
    fn short_long_and_wrong_type_adverts_are_dropped_and_a_long_one_read() {
        let rig = listening();
        let advert = ours(HEADER, PAYLOAD);
        let advertiser = rig
            .bluez()
            .advertise(&advert.address, &advert.data[..SHORT_LEN]);
        let mut wrong_type = advert.data;
        wrong_type[0] = WRONG_TYPE;
        advertiser.update(&wrong_type);
        rig.holds_for(SEEN_WITHIN, "nothing filled", |status| {
            status["case"]["available"] == false
        });
        let longer = [&advert.data[..], &TRAILING].concat();
        advertiser.update(&longer);
        rig.wait_status("the long advert in the status", |status| {
            picked(status) == filled()
        });
        assert_eq!(rig.bluez().calls(), registration(rig.adapter()));
        rig.finish();
    }

    #[test]
    #[expect(
        clippy::print_stderr,
        reason = "the daemon's resident memory before and after goes into the test's output"
    )]
    fn a_flood_of_adverts_logs_nothing_and_status_still_answers() {
        let mut rig = listening();
        let pid = rig.daemon().child.id();
        let log_before = rig.daemon().log();
        let before_kib = resident_kib(pid);
        let advert_from = |at: u8, round: u8| {
            let mut payload = PAYLOAD;
            payload[SEALED_LEN - 1] = round;
            advert_of(
                HEADER,
                payload,
                &keys(OTHER_IDENTITY),
                [FLOOD_RANDOM[0], FLOOD_RANDOM[1], at],
            )
        };
        let advertisers: Vec<_> = (0..FLOOD_ADDRESSES)
            .map(|at| {
                let advert = advert_from(at, 0);
                rig.bluez().advertise(&advert.address, &advert.data)
            })
            .collect();
        let rounds = FLOOD / usize::from(FLOOD_ADDRESSES);
        for round in 1..rounds {
            for (at, advertiser) in (0..FLOOD_ADDRESSES).zip(&advertisers) {
                advertiser.update(&advert_from(at, u8::try_from(round).unwrap()).data);
            }
        }
        // Asked once adverts are coming and answered before they stop, so it answered during the flood.
        let flooding = AtomicBool::new(true);
        let sent = AtomicUsize::new(0);
        let (status, stopped_by_status) = thread::scope(|scope| {
            let flood = scope.spawn(|| {
                let started = Instant::now();
                let mut round = u8::try_from(rounds).unwrap();
                while flooding.load(Ordering::Relaxed) {
                    if started.elapsed() >= HANG_LIMIT {
                        return false;
                    }
                    for (at, advertiser) in (0..FLOOD_ADDRESSES).zip(&advertisers) {
                        advertiser.update(&advert_from(at, round).data);
                        sent.fetch_add(1, Ordering::Relaxed);
                    }
                    round = round.wrapping_add(1);
                }
                true
            });
            wait_for("the flood under way", || {
                (sent.load(Ordering::Relaxed) > 0).then_some(())
            });
            let status = rig.status();
            flooding.store(false, Ordering::Relaxed);
            (status, flood.join().unwrap())
        });
        assert!(
            stopped_by_status,
            "the flood's deadline ended it, not the status"
        );
        assert_eq!(status["case"]["available"], false);
        let advert = ours(HEADER, PAYLOAD);
        let advertiser = rig.bluez().advertise(&advert.address, &advert.data);
        let mut round = 0_u8;
        // Sent changed in its last sealed byte, which no reading uses, until shown: a read or a queued advert can each be dropped at ADVERT_QUEUE.
        wait_for("our advert after the flood", || {
            let shown = picked(&rig.status()) == filled();
            if !shown {
                round = round.wrapping_add(1);
                let mut payload = PAYLOAD;
                payload[SEALED_LEN - 1] = round;
                advertiser.update(&ours(HEADER, payload).data);
            }
            shown.then_some(())
        });
        let after_kib = resident_kib(pid);
        eprintln!("resident memory: {before_kib} KiB before the flood, {after_kib} KiB after");
        assert!(after_kib.saturating_sub(before_kib) <= RSS_GROWTH_MAX_KIB);
        assert_eq!(rig.daemon().log(), log_before);
        assert_eq!(rig.bluez().calls(), registration(rig.adapter()));
        rig.finish();
    }

    #[test]
    fn an_advertiser_is_never_shown_as_a_device() {
        let rig = listening();
        let advert = ours(HEADER, PAYLOAD);
        let _advertiser = rig.bluez().advertise(&advert.address, &advert.data);
        let status = rig.wait_status("the advert in the status", |status| {
            picked(status) == filled()
        });
        assert_eq!(status["device_name"], "");
        assert_eq!(status["connected"], false);
        let mut client = Proto1::connect(rig.session());
        assert_eq!(
            outcome(&client.ask(&do_request(1, "disconnect"))),
            "no_device"
        );
        rig.no_dial_for(QUIET);
        assert_eq!(rig.bluez().calls(), registration(rig.adapter()));
        rig.finish();
    }
}
