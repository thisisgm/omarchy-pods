//! The built daemon arms dictation through the trap and the fake pods on both channels.

#[cfg(test)]
#[expect(dead_code, reason = "each suite uses only part of the shared support")]
mod support;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, Instant};

    use pods_fake::{Profile, doap::Table};
    use pods_proto::packets::{ATT_PSM, CONTROL_PSM};
    use pods_samples::{DOAP_MODE, MODEL_NUMBER, NAME, Placed, setting};
    use pods_trap::{Answer, Dial, Outcome};
    use rustix::io::Errno;
    use serde_json::Value;
    use tokio::runtime::Builder;
    use zbus::OwnedVariant;

    use super::support::bus::{BUS_NAME, INTERFACE, OBJECT_PATH, in_time, owned_connection};
    use super::support::doap::route;
    use super::support::pipewire;
    use super::support::players::PROPERTIES;
    use super::support::rig::{Device, REAL_TIME_SLACK, Rig, assert_gap};
    use super::support::settings::set;
    use super::support::uplink::{keep_settings, read_up_to_now, starts};
    use super::support::wait_for;

    const FIRST_RETRY: Duration = Duration::from_millis(750);
    const LAST_RETRY: Duration = Duration::from_millis(3000);
    const RETRY_STEPS: [Duration; 3] = [FIRST_RETRY, Duration::from_millis(1500), LAST_RETRY];
    const MTU_REQUEST: u8 = 0x02;
    const DICTATION_MODE_VALUES: [u8; 2] = [1, 2];
    const REFUSAL_AND_CONFIRMATION: (u32, u32) = (1, 1);

    fn rig(models: &[&str]) -> (Rig, Device) {
        let mut rig = Rig::new();
        keep_settings(rig.session(), "auto", models);
        let device = rig.add_pods(Profile::default());
        device.pods.doap().table(Table::Observed);
        rig.start();
        (rig, device)
    }

    fn delay_arm(rig: &Rig, device: &Device) {
        let mut att = route(device);
        let Answer::Accept { after, .. } = &mut att.answer else {
            panic!("expected the fake pods to accept the attribute channel");
        };
        *after = FIRST_RETRY;
        rig.route(att);
    }

    fn wait_status(rig: &Rig, word: &str) {
        wait_for(&format!("mic_dictation to read {word}"), || {
            (rig.status()["mic_dictation"] == word).then_some(())
        });
    }

    fn wait_armed(rig: &Rig, device: &Device, selections: u32) {
        wait_for("all CCCDs followed by SelectCodec", || {
            (device.pods.doap().selections() == selections).then_some(())
        });
        wait_status(rig, "armed");
    }

    fn no_start_or_mode(rig: &Rig, device: &Device) {
        read_up_to_now(rig, device);
        assert_eq!(device.pods.doap().starts(), 0);
        assert!(
            !device.pods.heard().iter().any(|heard| {
                DICTATION_MODE_VALUES
                    .into_iter()
                    .any(|mode| heard.sdu == setting(DOAP_MODE, mode))
            }),
            "the daemon wrote the mode on the control channel"
        );
    }

    fn dials(rig: &Rig, psm: u16) -> Vec<Dial> {
        rig.all_dials()
            .into_iter()
            .filter(|dial| dial.psm == psm)
            .collect()
    }

    #[test]
    fn the_link_up_arms_and_sends_no_mode() {
        let (rig, device) = rig(&[MODEL_NUMBER]);
        rig.link_up(&device);
        wait_armed(&rig, &device, 1);
        device.pods.doap().send_request(MTU_REQUEST);
        device.pods.doap().indicate();
        wait_for("the refusal and confirmation on the armed channel", || {
            (device.pods.doap().answered() == REFUSAL_AND_CONFIRMATION).then_some(())
        });
        rig.no_bluetooth_dial_for(FIRST_RETRY);
        no_start_or_mode(&rig, &device);
        assert_eq!(dials(&rig, ATT_PSM).len(), 1);
        assert_eq!(dials(&rig, CONTROL_PSM).len(), 1);
        rig.finish();
    }

    #[test]
    fn mic_dictation_reads_armed() {
        let (mut rig, device) = rig(&[MODEL_NUMBER]);
        delay_arm(&rig, &device);
        wait_status(&rig, "off");
        rig.link_up(&device);
        wait_status(&rig, "arming");
        wait_armed(&rig, &device, 1);
        wait_for("the state file to read armed", || {
            let file = fs::read(rig.session().state_file()).ok()?;
            let status: Value = serde_json::from_slice(&file).ok()?;
            (status["mic_dictation"] == "armed").then_some(())
        });
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        in_time(&runtime, async {
            let address = rig.session_bus().to_owned();
            let bus = owned_connection(&address, rig.daemon()).await;
            let message = bus
                .call_method(
                    Some(BUS_NAME),
                    OBJECT_PATH,
                    Some(PROPERTIES),
                    "Get",
                    &(INTERFACE, "MicDictation"),
                )
                .await
                .unwrap();
            let word: OwnedVariant = message.body().deserialize().unwrap();
            assert_eq!(String::try_from(word).unwrap(), "armed");
        });
        rig.finish();
    }

    #[test]
    fn a_reconnect_arms_again_and_sends_no_mode() {
        let (rig, device) = rig(&[MODEL_NUMBER]);
        rig.link_up(&device);
        wait_armed(&rig, &device, 1);
        rig.link_down(&device);
        wait_status(&rig, "off");
        rig.link_up(&device);
        wait_armed(&rig, &device, 2);
        no_start_or_mode(&rig, &device);
        assert_eq!(dials(&rig, ATT_PSM).len(), 2);
        assert_eq!(dials(&rig, CONTROL_PSM).len(), 2);
        rig.finish();
    }

    #[test]
    fn a_table_at_other_handles_is_found_by_uuid() {
        let (rig, device) = rig(&[MODEL_NUMBER]);
        device.pods.doap().table(Table::Shifted);
        rig.link_up(&device);
        wait_armed(&rig, &device, 1);
        no_start_or_mode(&rig, &device);
        rig.finish();
    }

    #[test]
    fn no_doap_service_reads_unavailable_and_no_start_is_ever_sent() {
        let (mut rig, device) = rig(&[MODEL_NUMBER]);
        device.pods.doap().table(Table::Absent);
        rig.link_up(&device);
        wait_status(&rig, "unavailable");
        rig.wait_logged("dictation: unavailable (no_service)");
        wait_for("the unavailable ATT channel to close", || {
            (device.pods.doap().channels() == 0).then_some(())
        });
        rig.no_bluetooth_dial_for(FIRST_RETRY);
        no_start_or_mode(&rig, &device);
        assert_eq!(dials(&rig, ATT_PSM).len(), 1);
        rig.finish();
    }

    #[test]
    fn a_refused_channel_is_tried_on_the_ladder_then_unavailable() {
        let (mut rig, device) = rig(&[MODEL_NUMBER]);
        let mut att = route(&device);
        att.answer = Answer::Refuse {
            after: Duration::ZERO,
            errno: Errno::CONNREFUSED,
        };
        rig.route(att);
        rig.link_up(&device);
        wait_status(&rig, "unavailable");
        rig.wait_logged("dictation: unavailable (bearer)");
        let attempts = dials(&rig, ATT_PSM);
        assert_eq!(attempts.len(), RETRY_STEPS.len().saturating_add(1));
        assert!(
            attempts
                .iter()
                .all(|dial| dial.outcome == Outcome::Refused(Errno::CONNREFUSED))
        );
        let pairs = attempts.iter().zip(attempts.iter().skip(1));
        for ((earlier, later), spacing) in pairs.zip(RETRY_STEPS) {
            assert_gap(
                "dictation bearer retry",
                earlier.at,
                later.at,
                &(spacing..=spacing + REAL_TIME_SLACK),
            );
        }
        rig.no_bluetooth_dial_for(LAST_RETRY + REAL_TIME_SLACK);
        no_start_or_mode(&rig, &device);
        rig.finish();
    }

    #[test]
    fn the_channel_lost_while_armed_arms_again() {
        let (mut rig, device) = rig(&[MODEL_NUMBER]);
        rig.link_up(&device);
        wait_armed(&rig, &device, 1);
        let hangup = Instant::now();
        device.pods.hang_up_att();
        wait_armed(&rig, &device, 2);
        rig.wait_logged("dictation: bearer lost, arming again");
        let attempts = dials(&rig, ATT_PSM);
        assert_eq!(attempts.len(), 2);
        assert_gap(
            "dictation bearer recovery",
            hangup,
            attempts[1].at,
            &(FIRST_RETRY..=FIRST_RETRY + REAL_TIME_SLACK),
        );
        assert_eq!(dials(&rig, CONTROL_PSM).len(), 1);
        no_start_or_mode(&rig, &device);
        rig.finish();
    }

    #[test]
    fn the_preference_off_closes_the_channel_and_reads_off() {
        let (rig, device) = rig(&[MODEL_NUMBER]);
        rig.link_up(&device);
        wait_armed(&rig, &device, 1);
        set(rig.session(), "microphone", "off");
        wait_status(&rig, "off");
        wait_for("the disabled ATT channel to close", || {
            (device.pods.doap().channels() == 0).then_some(())
        });
        assert_eq!(rig.status()["connected"], true);
        rig.no_bluetooth_dial_for(FIRST_RETRY);
        no_start_or_mode(&rig, &device);
        set(rig.session(), "microphone", "auto");
        wait_armed(&rig, &device, 2);
        assert_eq!(dials(&rig, CONTROL_PSM).len(), 1);
        rig.finish();
    }

    #[test]
    fn an_armed_model_is_confirmed_without_an_uplink_start() {
        let (rig, device) = rig(&[]);
        let pipewire = pipewire::start(&rig);
        delay_arm(&rig, &device);
        device.pods.set_ear(Placed::InEar, Placed::InEar);
        rig.link_up(&device);
        wait_armed(&rig, &device, 1);
        pipewire::wait_node(&pipewire);
        wait_for("the armed model to be kept in settings", || {
            let settings = fs::read(rig.session().settings_file()).ok()?;
            let settings: Value = serde_json::from_slice(&settings).ok()?;
            (settings["mic_models"][0] == MODEL_NUMBER).then_some(())
        });
        no_start_or_mode(&rig, &device);
        assert!(starts(&device).is_empty(), "the daemon started the uplink");
        rig.finish();
    }

    #[test]
    fn the_log_holds_no_address_and_no_name() {
        let (mut rig, device) = rig(&[MODEL_NUMBER]);
        rig.link_up(&device);
        wait_armed(&rig, &device, 1);
        device.pods.hang_up_att();
        wait_armed(&rig, &device, 2);
        device.pods.doap().table(Table::Absent);
        device.pods.hang_up_att();
        wait_status(&rig, "unavailable");
        rig.wait_logged("dictation: unavailable (no_service)");
        let log = rig.daemon().log();
        assert!(
            log.lines()
                .any(|line| line.starts_with("dictation: armed after "))
        );
        let log = log.to_lowercase();
        let octets = device.address.octets().map(|octet| format!("{octet:02x}"));
        for sensitive in [octets.join("_"), octets.join(":"), NAME.to_lowercase()] {
            assert!(
                !log.contains(&sensitive),
                "the log contains a device identifier"
            );
        }
        rig.finish();
    }
}
