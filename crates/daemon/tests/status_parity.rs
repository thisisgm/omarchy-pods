//! One status on every surface of the built daemon: `librepods status`, the legacy `librepods ctl status`, the state file and the D-Bus interface.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet, HashMap};
    use std::fs;

    use cpp_source::MAIN;
    use pods_core::command::NoiseTarget;
    use pods_core::state::{EarDetectionBehavior, State};
    use pods_core::status::{ADDITIVE_KEYS, CPP_KEY_COUNT, to_status_object};
    use pods_fake::Profile;
    use pods_proto::noise::NoiseMode;
    use pods_samples::NAME;
    use serde_json::Value;
    use zbus::{OwnedVariant, Variant};

    use super::support::advert::{HEADER, PAYLOAD, keyed, ours};
    use super::support::bus::{BUS_NAME, INTERFACE, Listener, OBJECT_PATH, changes_rule};
    use super::support::players::PROPERTIES;
    use super::support::rig::{Rig, connected, named};
    use super::support::wait_for;

    /// The mark in the sample name, which every surface must carry as raw UTF-8.
    const MARK: char = '\u{2019}';
    /// The constant property, which the document does not carry.
    const API_VERSION: &str = "ApiVersion";
    /// The document's prefix for the flags `Capabilities` lists.
    const CAPABILITY_PREFIX: &str = "supports_";

    /// The C++ daemon's status keys in the order it inserts them; sample input `status.insert("schema_version", 1);`.
    fn cpp_keys() -> Vec<&'static str> {
        MAIN.body("QJsonObject statusJson(")
            .split("status.insert(\"")
            .skip(1)
            .filter_map(|rest| rest.split_once('"'))
            .map(|(key, _)| key)
            .collect()
    }

    /// The C++ keys and the ones this daemon adds.
    fn document_keys() -> BTreeSet<&'static str> {
        cpp_keys()
            .into_iter()
            .chain(ADDITIVE_KEYS.iter().copied())
            .collect()
    }

    /// Each surface as it was read, bytes as they came.
    struct Surfaces {
        command_line: Vec<u8>,
        legacy: Vec<u8>,
        state_file: Vec<u8>,
        properties: HashMap<String, OwnedVariant>,
    }

    /// A client per read, so its stream's queue holds only the signals of one call's round trip.
    fn read(rig: &Rig) -> Surfaces {
        let listener = Listener::subscribe(rig.session_bus(), &changes_rule());
        let printed = |args: &[&str]| {
            let output = rig.client(args);
            assert!(output.status.success(), "librepods {args:?}: {output:?}");
            output.stdout
        };
        let command_line = printed(&["status"]);
        let legacy = printed(&["ctl", "status"]);
        let state_file = fs::read(rig.session().state_file()).unwrap_or_default();
        let reply = listener.call(BUS_NAME, OBJECT_PATH, PROPERTIES, "GetAll", &(INTERFACE,));
        let properties = reply
            .ok()
            .and_then(|reply| reply.body().deserialize().ok())
            .unwrap_or_default();
        Surfaces {
            command_line,
            legacy,
            state_file,
            properties,
        }
    }

    fn text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    /// Every property but `ApiVersion`, as the document gives it.
    fn expected_properties(
        document: &Value,
        line: &str,
    ) -> BTreeMap<&'static str, Variant<'static>> {
        let flag = |value: &Value| value.as_bool().unwrap_or_default();
        let word = |key: &str| document[key].as_str().unwrap_or_default().to_owned();
        let byte = |value: &Value| {
            value
                .as_u64()
                .and_then(|number| u8::try_from(number).ok())
                .unwrap_or(u8::MAX)
        };
        let battery = |side: &Value| {
            (
                flag(&side["available"]),
                flag(&side["charging"]),
                byte(&side["level"]),
            )
        };
        let noise = NoiseMode::try_from(byte(&document["noise_mode"]))
            .map_or("", |mode| NoiseTarget::Mode(mode).word());
        let ear = EarDetectionBehavior::ALL
            .into_iter()
            .find(|behavior| {
                let numbered = to_status_object(&State {
                    ear_detection_behavior: *behavior,
                    ..State::default()
                });
                numbered["ear_detection_behavior"] == document["ear_detection_behavior"]
            })
            .map_or("", EarDetectionBehavior::word);
        let batteries: BTreeMap<String, (bool, bool, u8)> = ["case", "headset", "left", "right"]
            .into_iter()
            .map(|side| (side.to_owned(), battery(&document[side])))
            .collect();
        let in_ear: BTreeMap<String, bool> = ["left", "right"]
            .into_iter()
            .map(|side| (side.to_owned(), flag(&document[side]["in_ear"])))
            .collect();
        let capabilities: Vec<String> = document
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(_, value)| **value == Value::Bool(true))
            .filter_map(|(key, _)| key.strip_prefix(CAPABILITY_PREFIX))
            .map(str::to_owned)
            .collect();
        BTreeMap::from([
            ("Connected", Variant::from(flag(&document["connected"]))),
            ("DeviceName", Variant::from(word("device_name"))),
            ("ModelName", Variant::from(word("model_name"))),
            ("NoiseMode", Variant::from(noise)),
            ("EarDetection", Variant::from(ear)),
            (
                "ConversationalAwareness",
                Variant::from(flag(&document["conversational_awareness"])),
            ),
            (
                "OneBudAnc",
                Variant::from(flag(&document["one_bud_anc_mode"])),
            ),
            (
                "AdaptiveLevel",
                Variant::from(byte(&document["adaptive_noise_level"])),
            ),
            ("Batteries", Variant::from(batteries)),
            ("InEar", Variant::from(in_ear)),
            ("Capabilities", Variant::from(capabilities)),
            ("StatusJson", Variant::from(line.trim_end().to_owned())),
            ("AudioServer", Variant::from(word("audio_server"))),
            ("AdvertIntake", Variant::from(word("advert_intake"))),
            ("Microphone", Variant::from(word("microphone"))),
            ("Mic", Variant::from(word("mic"))),
        ])
    }

    /// What one read of the four surfaces shows apart from `librepods status`, one line each.
    fn disagreements(surfaces: &Surfaces) -> Vec<String> {
        let line = text(&surfaces.command_line);
        let Ok(document) = serde_json::from_str::<Value>(&line) else {
            return vec![format!("librepods status printed {line:?}")];
        };
        let mut found = Vec::new();
        let keys: BTreeSet<&str> = document
            .as_object()
            .into_iter()
            .flatten()
            .map(|(key, _)| key.as_str())
            .collect();
        if keys != document_keys() {
            found.push(format!("librepods status carries the keys {keys:?}"));
        }
        for (name, bytes) in [
            ("librepods ctl status", &surfaces.legacy),
            ("the state file", &surfaces.state_file),
        ] {
            if serde_json::from_slice::<Value>(bytes).ok().as_ref() != Some(&document) {
                found.push(format!("{name} shows {}, not {line}", text(bytes)));
            }
        }
        let expected = expected_properties(&document, &line);
        for (property, wanted) in &expected {
            match surfaces.properties.get(*property) {
                Some(got) if **got == *wanted => {}
                got => found.push(format!("D-Bus {property} is {got:?}, not {wanted:?}")),
            }
        }
        let unexpected: Vec<&String> = surfaces
            .properties
            .keys()
            .filter(|name| *name != API_VERSION && !expected.contains_key(name.as_str()))
            .collect();
        if !unexpected.is_empty() {
            found.push(format!(
                "D-Bus carries properties no key gives: {unexpected:?}"
            ));
        }
        found
    }

    /// Reads the four surfaces until they agree and gives that read; each new disagreement goes to standard error, so a timeout shows what still differed.
    #[expect(
        clippy::print_stderr,
        reason = "a timeout shows what still differed between the surfaces"
    )]
    fn agreed(rig: &Rig) -> (Value, Surfaces) {
        let mut last = Vec::new();
        wait_for("the four surfaces agreeing", || {
            let surfaces = read(rig);
            let found = disagreements(&surfaces);
            if found.is_empty() {
                let document = serde_json::from_slice(&surfaces.command_line).unwrap();
                return Some((document, surfaces));
            }
            if found != last {
                eprintln!("{}", found.join("\n"));
                last = found;
            }
            None
        })
    }

    #[test]
    fn the_four_surfaces_agree_after_the_burst() {
        let (rig, _device) = Rig::with_linked_pods();
        rig.wait_status("both buds read", |status| {
            status["left"]["available"] == true && status["right"]["available"] == true
        });
        let (document, _) = agreed(&rig);
        assert!(connected(&document) && named(&document), "{document}");
        rig.finish();
    }

    #[test]
    fn they_agree_after_an_advert_only_state() {
        let mut rig = keyed();
        rig.add_pods(Profile::default());
        rig.start();
        rig.wait_status("advert_intake listening", |status| {
            status["advert_intake"] == "listening"
        });
        let advert = ours(HEADER, PAYLOAD);
        let advertiser = rig.bluez().advertise(&advert.address, &advert.data);
        rig.wait_status("the case read from the advert", |status| {
            status["case"]["available"] == true
        });
        let (document, _) = agreed(&rig);
        assert!(!connected(&document), "{document}");
        assert_eq!(document["case"]["available"], true, "{document}");
        advertiser.lose();
        rig.finish();
    }

    #[test]
    fn they_agree_after_a_loss() {
        let (rig, device) = Rig::with_linked_pods();
        rig.wait_status("both buds read", |status| {
            status["left"]["available"] == true && status["right"]["available"] == true
        });
        rig.link_down(&device);
        rig.wait_status("the reset after the loss", |status| {
            !connected(status) && !named(status)
        });
        let (document, _) = agreed(&rig);
        assert_eq!(document["left"]["available"], false, "{document}");
        rig.finish();
    }

    #[test]
    fn the_name_with_u2019_is_raw_utf8_on_every_surface() {
        assert_eq!(NAME.matches(MARK).count(), 1, "{NAME}");
        let mut mark = [0; 4];
        let mark = MARK.encode_utf8(&mut mark).as_bytes();
        let (rig, _device) = Rig::with_linked_pods();
        let (_, surfaces) = agreed(&rig);
        let status_json = match surfaces.properties.get("StatusJson") {
            Some(value) => String::try_from(value.try_clone().unwrap()).unwrap(),
            None => panic!("no StatusJson property"),
        };
        for (name, bytes) in [
            ("librepods status", surfaces.command_line.as_slice()),
            ("librepods ctl status", surfaces.legacy.as_slice()),
            ("the state file", surfaces.state_file.as_slice()),
            ("StatusJson", status_json.as_bytes()),
        ] {
            let high: Vec<u8> = bytes.iter().copied().filter(|byte| *byte > 127).collect();
            assert_eq!(high, mark, "{name}: {}", text(bytes));
            assert!(!text(bytes).contains("\\u"), "{name}: {}", text(bytes));
            assert!(text(bytes).contains(NAME), "{name}: {}", text(bytes));
        }
        rig.finish();
    }

    #[test]
    fn the_36_old_keys_are_all_still_there() {
        let old = cpp_keys();
        assert_eq!(old.len(), CPP_KEY_COUNT, "{old:?}");
        assert_eq!(
            document_keys().len(),
            CPP_KEY_COUNT + ADDITIVE_KEYS.len(),
            "an added key shares a name with an old one"
        );
        let mut rig = Rig::new();
        rig.start();
        let (document, _) = agreed(&rig);
        for key in old {
            assert!(document.get(key).is_some(), "{key}: {document}");
        }
        rig.finish();
    }
}
