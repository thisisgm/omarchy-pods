//! What a device is, through the built daemon: the Off option the pods report, the models, an unknown model, and Beats.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use pods_core::state::State;
    use pods_core::status::{ADDITIVE_KEYS, CPP_KEY_COUNT, to_status_object};
    use pods_fake::Profile;
    use pods_samples::advert::{HEADER_LEN, PRODUCT_AT};
    use pods_samples::{ALLOW_OFF_OPTION, NOISE, setting};
    use serde_json::{Value, json};
    use zbus::{OwnedVariant, Str};

    use super::support::advert::{HEADER, PAYLOAD, PRIMARY_LEVEL, keyed, ours};
    use super::support::rig::{ANC, DIAL_WAIT, Device, OFF, Rig, TRANSPARENCY, connected};
    use super::support::{HANG_LIMIT, Proto1, outcome, set_request, wait_for};

    /// Powerbeats Pro, product `0x200B`, as BlueZ prints it.
    const BEATS: &str = "bluetooth:v004Cp200Bd0001";
    /// Beats Studio Pro, product `0x2017`.
    const STUDIO_PRO: &str = "bluetooth:v004Cp2017d0001";
    /// Beats Solo 4, product `0x2025`.
    const SOLO_4: &str = "bluetooth:v004Cp2025d0001";
    /// An Apple product in neither list.
    const UNKNOWN_APPLE: &str = "bluetooth:v004Cp20FFd0001";
    /// The Powerbeats Pro product number under another vendor.
    const OTHER_VENDOR: &str = "bluetooth:v0059p200Bd0001";
    const CUT: &str = "bluetooth:v004Cp200B";
    const NOT_AIRPODS_LOG: &str = "bluez: one device is not AirPods";
    /// AirPods Pro 3, whose table entry offers no Off.
    const TABLE_WITHOUT_OFF: &str = "A3063";
    /// AirPods Pro 2 (USB-C), whose table entry offers Off.
    const TABLE_WITH_OFF: &str = "A3048";
    const UNKNOWN_MODEL: &str = "A0000";
    /// Modalias, which decides only the dial, the model number the pods report, then the name, `model_int` and headset flag it gives.
    const NAMED_MODELS: [(&str, &str, &str, i64, bool); 4] = [
        ("bluetooth:v004Cp2036d0001", "A3533", "AirPods 5", 13, false),
        ("bluetooth:v004Cp2030d0001", "A3440", "AirPods 5", 14, false),
        ("bluetooth:v004Cp2030d0001", "A3533", "AirPods 5", 13, false),
        (
            "bluetooth:v004Cp202Dd0001",
            "A3454",
            "AirPods Max 2",
            12,
            true,
        ),
    ];
    /// The two product bytes of an advert, low byte first, then the name, `model_int` and headset flag the status shows.
    const ADVERT_MODELS: [([u8; 2], &str, i64, bool); 3] = [
        ([0x36, 0x20], "AirPods 5", 13, false),
        ([0x30, 0x20], "AirPods 5", 14, false),
        ([0x2d, 0x20], "AirPods Max 2", 12, true),
    ];
    const OFFERED: u8 = 1;
    const NOT_OFFERED: u8 = 2;
    /// Pods of `model_number` in noise cancelling, so a change to Off is a real write.
    fn pods(model_number: &str) -> Profile {
        Profile {
            model_number: model_number.to_owned(),
            noise: ANC.wire,
            ..Profile::default()
        }
    }

    /// The daemon started with one pair of pods BlueZ holds connected, under `modalias` when given.
    fn started(profile: Profile, modalias: Option<&str>) -> (Rig, Device) {
        let mut rig = Rig::new();
        let device = rig.add_pods(profile);
        if let Some(modalias) = modalias {
            set_modalias(&rig, &device, modalias);
        }
        rig.link_up(&device);
        rig.start();
        (rig, device)
    }

    fn set_modalias(rig: &Rig, device: &Device, modalias: &str) {
        let value = OwnedVariant::from(Str::from(modalias.to_owned()));
        rig.bluez().set(&device.path, "Modalias", value);
    }

    fn ask_off(rig: &Rig) -> String {
        let reply = Proto1::connect(rig.session()).ask(&set_request(1, "noise", "off"));
        outcome(&reply).to_owned()
    }

    fn wait_offers_off(rig: &Rig, offered: bool) -> Value {
        rig.wait_status(&format!("supports_noise_off {offered}"), |status| {
            status["supports_noise_off"] == offered
        })
    }

    fn sorted_keys(status: &Value) -> Vec<String> {
        let mut keys: Vec<String> = status.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    }

    #[test]
    fn a_device_that_reports_the_off_option_offers_noise_off() {
        let (rig, device) = started(pods(TABLE_WITHOUT_OFF), None);
        assert_eq!(rig.wait_filled()["supports_noise_off"], false);
        device.pods.set_setting(ALLOW_OFF_OPTION, OFFERED);
        wait_offers_off(&rig, true);
        assert_eq!(ask_off(&rig), "sent");
        assert_eq!(rig.status()["noise_mode"], OFF.status);
        rig.finish();
    }

    #[test]
    fn a_report_of_no_off_option_refuses_noise_off() {
        let (rig, device) = started(pods(TABLE_WITH_OFF), None);
        assert_eq!(rig.wait_filled()["supports_noise_off"], true);
        device.pods.set_setting(ALLOW_OFF_OPTION, NOT_OFFERED);
        wait_offers_off(&rig, false);
        assert_eq!(ask_off(&rig), "not_supported");
        assert_eq!(rig.status()["noise_mode"], ANC.status);
        let reply = Proto1::connect(rig.session()).ask(&set_request(2, "noise", "transparency"));
        assert_eq!(outcome(&reply), "sent", "{reply}");
        // The channel keeps order, so once the later write is heard a refused Off frame would be too.
        let transparency = setting(NOISE, TRANSPARENCY.wire);
        device
            .pods
            .wait_heard(&transparency, 1, HANG_LIMIT)
            .unwrap();
        let off = setting(NOISE, OFF.wire);
        assert!(device.pods.heard().iter().all(|heard| heard.sdu != off));
        rig.finish();
    }

    #[test]
    fn without_the_report_the_model_table_decides() {
        for (model_number, offered, answer) in [
            (TABLE_WITHOUT_OFF, false, "not_supported"),
            (TABLE_WITH_OFF, true, "sent"),
        ] {
            let (rig, _device) = started(pods(model_number), None);
            assert_eq!(
                rig.wait_filled()["supports_noise_off"],
                offered,
                "{model_number}"
            );
            assert_eq!(ask_off(&rig), answer, "{model_number}");
            rig.finish();
        }
    }

    #[test]
    fn the_report_does_not_outlive_the_link() {
        let (rig, device) = started(pods(TABLE_WITHOUT_OFF), None);
        rig.wait_filled();
        device.pods.set_setting(ALLOW_OFF_OPTION, OFFERED);
        wait_offers_off(&rig, true);
        rig.link_down(&device);
        rig.wait_status("the device forgotten after the grace", |status| {
            !connected(status) && status["model_int"] == 0
        });
        rig.link_up(&device);
        assert_eq!(rig.wait_filled()["supports_noise_off"], false);
        assert_eq!(ask_off(&rig), "not_supported");
        rig.finish();
    }

    #[test]
    fn an_unknown_model_number_is_a_generic_device() {
        let (rig, _device) = started(pods(UNKNOWN_MODEL), None);
        let status = rig.wait_filled();
        assert_eq!(
            sorted_keys(&status),
            sorted_keys(&to_status_object(&State::default()))
        );
        assert_eq!(
            status.as_object().unwrap().len(),
            CPP_KEY_COUNT + ADDITIVE_KEYS.len()
        );
        let generic = json!({
            "model_number": UNKNOWN_MODEL,
            "model_name": "",
            "model_int": 0,
            "is_pro_series": false,
            "is_headset": false,
            "supports_noise_off": true,
            "supports_noise_control": true,
            "supports_adaptive": false,
            "supports_conversational_awareness": false,
            "supports_one_bud_anc": false,
        });
        for (key, value) in generic.as_object().unwrap() {
            assert_eq!(&status[key], value, "{key}");
        }
        rig.finish();
    }

    #[test]
    fn an_airpods_5_or_max_2_modalias_is_dialled_and_named_from_its_number() {
        for (modalias, number, name, int, headset) in NAMED_MODELS {
            let (rig, _device) = started(pods(number), Some(modalias));
            let status = rig.wait_filled();
            let wanted = json!({
                "model_number": number,
                "model_name": name,
                "model_int": int,
                "is_pro_series": false,
                "is_headset": headset,
                "supports_noise_off": true,
                "supports_noise_control": true,
                "supports_adaptive": true,
                "supports_conversational_awareness": true,
                "supports_one_bud_anc": !headset,
            });
            for (key, value) in wanted.as_object().unwrap() {
                assert_eq!(&status[key], value, "{modalias} {number} {key}");
            }
            rig.finish();
        }
    }

    #[test]
    fn an_advert_names_airpods_5_and_max_2_while_the_link_is_down() {
        for (word, name, int, headset) in ADVERT_MODELS {
            let mut rig = keyed();
            rig.add_pods(Profile::default());
            rig.start();
            rig.wait_status("advert_intake listening", |status| {
                status["advert_intake"] == "listening"
            });
            let mut header: [u8; HEADER_LEN] = HEADER;
            header[PRODUCT_AT..PRODUCT_AT + word.len()].copy_from_slice(&word);
            let advert = ours(header, PAYLOAD);
            let _advertiser = rig.bluez().advertise(&advert.address, &advert.data);
            let status = rig.wait_status("the advert's battery", |status| {
                status["left"]["available"] == true || status["headset"]["available"] == true
            });
            assert_eq!(status["model_name"], name, "{word:02x?}");
            assert_eq!(status["model_int"], int, "{word:02x?}");
            assert_eq!(status["headset"]["available"], headset, "{word:02x?}");
            let primary = if headset { "headset" } else { "left" };
            assert_eq!(status[primary]["level"], PRIMARY_LEVEL, "{word:02x?}");
            for component in ["left", "right", "case"] {
                assert_eq!(
                    status[component]["available"], !headset,
                    "{word:02x?} {component}"
                );
            }
            assert!(!connected(&status), "{word:02x?}");
            rig.finish();
        }
    }

    #[test]
    fn a_beats_modalias_is_never_dialled() {
        for modalias in [BEATS, STUDIO_PRO, SOLO_4] {
            let (mut rig, _device) = started(pods(TABLE_WITH_OFF), Some(modalias));
            wait_for("a sweep", || {
                (!rig.bluez().sweeps().is_empty()).then_some(())
            });
            wait_for("the line for the Beats", || {
                assert_eq!(rig.dials().len(), 0, "{modalias} dialled");
                rig.daemon().log().contains(NOT_AIRPODS_LOG).then_some(())
            });
            rig.no_dial_for(DIAL_WAIT);
            assert_eq!(rig.dials().len(), 0, "{modalias}");
            let status = rig.status();
            assert!(!connected(&status), "{modalias} {status}");
            assert_eq!(status["device_name"], "", "{modalias} {status}");
            rig.finish();
        }
    }

    #[test]
    fn a_beats_modalias_after_the_uuids_is_never_dialled() {
        let mut rig = Rig::new();
        let device = rig.add_pods(pods(TABLE_WITH_OFF));
        rig.start();
        rig.wait_disconnect_answers("not_connected");
        set_modalias(&rig, &device, BEATS);
        wait_for("the line for the Beats", || {
            rig.daemon().log().contains(NOT_AIRPODS_LOG).then_some(())
        });
        rig.link_up(&device);
        rig.no_dial_for(DIAL_WAIT);
        assert_eq!(rig.dials().len(), 0);
        assert!(!connected(&rig.status()));
        rig.finish();
    }

    #[test]
    fn beats_are_logged_once_without_an_address() {
        let mut rig = Rig::new();
        for _ in 0..2 {
            let beats = rig.add_pods(pods(TABLE_WITH_OFF));
            set_modalias(&rig, &beats, BEATS);
            rig.link_up(&beats);
        }
        let airpods = rig.add_pods(pods(TABLE_WITH_OFF));
        rig.start();
        wait_for("the line for the Beats", || {
            rig.daemon().log().contains(NOT_AIRPODS_LOG).then_some(())
        });
        let swept = rig.bluez().sweeps().len();
        rig.bluez().drop_name();
        rig.bluez().take_name();
        wait_for("the sweep after BlueZ came back", || {
            (rig.bluez().sweeps().len() > swept).then_some(())
        });
        rig.link_up(&airpods);
        rig.wait_dials(1, HANG_LIMIT);
        let log = rig.daemon().log();
        let lines: Vec<&str> = log
            .lines()
            .filter(|line| line.contains(NOT_AIRPODS_LOG))
            .collect();
        assert_eq!(lines, [NOT_AIRPODS_LOG], "{log}");
        rig.finish();
    }

    #[test]
    fn an_unknown_apple_product_with_the_uuid_is_dialled() {
        let (rig, _device) = started(pods(TABLE_WITH_OFF), Some(UNKNOWN_APPLE));
        rig.wait_filled();
        rig.finish();
    }

    #[test]
    fn a_modalias_that_does_not_parse_changes_nothing() {
        let (rig, _device) = started(pods(TABLE_WITH_OFF), Some(CUT));
        rig.wait_filled();
        rig.finish();
    }

    #[test]
    fn another_vendor_with_the_uuid_is_treated_as_today() {
        let (rig, _device) = started(pods(TABLE_WITH_OFF), Some(OTHER_VENDOR));
        rig.wait_filled();
        rig.finish();
    }
}
