//! The interface over a peer to peer connection on a socket pair, so no bus daemon is needed.

mod document;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use pods_core::command::AdaptiveLevel;
use pods_core::status::to_status_line;
use pods_proto::battery::{Batteries, Charge};
use pods_proto::ear::{EarPlacement, EarReport};
use pods_proto::model::Model;
use tokio::runtime::Builder as RuntimeBuilder;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use zbus::export::futures_core::Stream;
use zbus::export::serde::Serialize;
use zbus::message::Type;
use zbus::names::MemberName;
use zbus::{DBusError, DynamicType, Guid, MessageStream, OwnedVariant};

use super::*;

/// A test still waiting after this long has hung, and fails.
const CEILING: Duration = Duration::from_secs(10);

/// The interface's name as clients write it.
const INTERFACE: &str = "io.github.thisisgm.Omapods1";

/// What every error name starts with.
const ERROR_PREFIX: &str = "io.github.thisisgm.Omapods1.Error.";

const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";
const INTROSPECTABLE_INTERFACE: &str = "org.freedesktop.DBus.Introspectable";
/// The public description of this interface, which must list every error name.
const API_DOCUMENT: &str = include_str!("../../../../../docs/dbus-api.md");

/// The error name a client sees for `error`, written out; a match with no catch-all, so a new variant fails to build until its name is spelled here.
fn spelled(error: CommandError) -> &'static str {
    match error {
        CommandError::NotConnected => "io.github.thisisgm.Omapods1.Error.NotConnected",
        CommandError::NotSupported => "io.github.thisisgm.Omapods1.Error.NotSupported",
        CommandError::NotWorn => "io.github.thisisgm.Omapods1.Error.NotWorn",
        CommandError::OutOfRange => "io.github.thisisgm.Omapods1.Error.OutOfRange",
        CommandError::UnknownKey => "io.github.thisisgm.Omapods1.Error.UnknownKey",
        CommandError::BadRequest => "io.github.thisisgm.Omapods1.Error.BadRequest",
        CommandError::Timeout => "io.github.thisisgm.Omapods1.Error.Timeout",
        CommandError::Headless => "io.github.thisisgm.Omapods1.Error.Headless",
    }
}

/// State changes the stalled peer test makes; their signals overfill a socket buffer many times over.
const FLOOD_CHANGES: usize = 5_000;

/// A quarter of the flood: a socket buffer holds a few hundred signals, and a queue would hold them all.
const MOST_SIGNALS_HELD: usize = FLOOD_CHANGES / 4;

/// The interface as `Introspect` describes it, compared after white space is removed.
const INTROSPECTED: &str = r#"
<interface name="io.github.thisisgm.Omapods1">
  <method name="SetNoiseMode">
    <arg name="mode" type="s" direction="in"/>
    <arg type="s" direction="out"/>
  </method>
  <method name="SetEarDetection">
    <arg name="behavior" type="s" direction="in"/>
    <arg type="s" direction="out"/>
  </method>
  <method name="SetConversationalAwareness">
    <arg name="enabled" type="b" direction="in"/>
    <arg type="s" direction="out"/>
  </method>
  <method name="SetOneBudAnc">
    <arg name="enabled" type="b" direction="in"/>
    <arg type="s" direction="out"/>
  </method>
  <method name="SetAdaptiveLevel">
    <arg name="level" type="y" direction="in"/>
    <arg type="s" direction="out"/>
  </method>
  <method name="Connect">
    <arg type="s" direction="out"/>
  </method>
  <method name="Disconnect">
    <arg type="s" direction="out"/>
  </method>
  <method name="Forget">
    <arg type="s" direction="out"/>
  </method>
  <property name="AdaptiveLevel" type="y" access="read"/>
  <property name="ApiVersion" type="u" access="read">
    <annotation name="org.freedesktop.DBus.Property.EmitsChangedSignal" value="const"/>
  </property>
  <property name="Batteries" type="a{s(bby)}" access="read"/>
  <property name="Capabilities" type="as" access="read"/>
  <property name="Connected" type="b" access="read"/>
  <property name="ConversationalAwareness" type="b" access="read"/>
  <property name="DeviceName" type="s" access="read"/>
  <property name="EarDetection" type="s" access="read"/>
  <property name="InEar" type="a{sb}" access="read"/>
  <property name="ModelName" type="s" access="read"/>
  <property name="NoiseMode" type="s" access="read"/>
  <property name="OneBudAnc" type="b" access="read"/>
  <property name="StatusJson" type="s" access="read"/>
</interface>
"#;

/// Runs one test on a current thread runtime like the daemon's, failing it once `CEILING` passes.
fn run<F: Future>(test: F) -> F::Output {
    RuntimeBuilder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async { timeout(CEILING, test).await.expect("the test hung") })
}

/// A client connected straight to the interface, with the announcing task the daemon would run.
struct Peer {
    hub: Hub,
    client: Connection,
    announcing: JoinHandle<()>,
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.announcing.abort();
    }
}

/// Subscribes as `serve` does, before the object is served.
fn subscribe(hub: &Hub) -> (watch::Receiver<State>, Properties) {
    let updates = hub.subscribe();
    let shown = Properties::of(&updates.borrow());
    (updates, shown)
}

async fn peer(hub: Hub) -> Peer {
    let (updates, shown) = subscribe(&hub);
    let (server_end, client_end) = UnixStream::pair().unwrap();
    let server = tokio::spawn(
        Builder::unix_stream(server_end)
            .server(Guid::generate())
            .p2p()
            .serve_at(OBJECT_PATH, Omapods { hub: hub.clone() })
            .build(),
    );
    let client = Builder::unix_stream(client_end)
        .p2p()
        .build()
        .await
        .unwrap();
    let server = server.await.unwrap().unwrap();
    let announcing = tokio::spawn(announce(server, updates, shown));
    Peer {
        hub,
        client,
        announcing,
    }
}

async fn get_all(client: &Connection) -> HashMap<String, OwnedVariant> {
    let reply = client
        .call_method(
            None::<&str>,
            OBJECT_PATH,
            Some(PROPERTIES_INTERFACE),
            "GetAll",
            &(INTERFACE,),
        )
        .await
        .unwrap();
    reply.body().deserialize().unwrap()
}

/// Calls `method`, giving back its reply or the name of its error.
async fn call<B: Serialize + DynamicType>(
    client: &Connection,
    method: &str,
    body: &B,
) -> Result<String, String> {
    match client
        .call_method(None::<&str>, OBJECT_PATH, Some(INTERFACE), method, body)
        .await
    {
        Ok(reply) => Ok(reply.body().deserialize::<String>().unwrap()),
        Err(zbus::Error::MethodError(name, _, _)) => Err(name.to_string()),
        Err(other) => panic!("{method} failed outside the interface: {other}"),
    }
}

/// The next `PropertiesChanged` on `stream`, as its changed properties.
async fn next_change(stream: &mut MessageStream) -> HashMap<String, OwnedVariant> {
    loop {
        let message = poll_fn(|context| Pin::new(&mut *stream).poll_next(context))
            .await
            .unwrap()
            .unwrap();
        let header = message.header();
        if header.message_type() != Type::Signal
            || header.member().map(MemberName::as_str) != Some("PropertiesChanged")
        {
            continue;
        }
        let (interface, changed, invalidated): (
            String,
            HashMap<String, OwnedVariant>,
            Vec<String>,
        ) = message.body().deserialize().unwrap();
        assert_eq!(interface, INTERFACE);
        assert!(invalidated.is_empty(), "{invalidated:?}");
        return changed;
    }
}

fn names(changed: &HashMap<String, OwnedVariant>) -> BTreeSet<&str> {
    changed.keys().map(String::as_str).collect()
}

fn text(value: &OwnedVariant) -> String {
    String::try_from(value.try_clone().unwrap()).unwrap()
}

fn populated() -> State {
    let mut state = State {
        connected: true,
        device_name: "Test Pods".to_owned(),
        model: Model::AirPodsPro2USBC,
        noise_mode: NoiseMode::Transparency,
        adaptive_noise_level: AdaptiveLevel::new(37).unwrap(),
        conversational_awareness: true,
        one_bud_anc_mode: true,
        batteries: Batteries {
            left: Some(Charge {
                level: 80,
                charging: false,
            }),
            right: Some(Charge {
                level: 60,
                charging: true,
            }),
            case: None,
            headset: None,
        },
        ear: EarReport {
            primary: EarPlacement::InEar,
            secondary: EarPlacement::NotInEar,
        },
        primary: Some(Component::Right),
        ear_detection_behavior: EarDetectionBehavior::PauseWhenBothRemoved,
        ..State::default()
    };
    state.counters.ear_detection_changes = 4;
    state
}

#[test]
fn properties_reflect_the_state() {
    run(async {
        let peer = peer(Hub::new()).await;
        peer.hub.replace(populated());
        let all = get_all(&peer.client).await;
        let value = |name: &str| all[name].try_clone().unwrap();
        assert_eq!(u32::try_from(value("ApiVersion")).unwrap(), 1);
        assert!(bool::try_from(value("Connected")).unwrap());
        assert_eq!(text(&value("DeviceName")), "Test Pods");
        assert_eq!(text(&value("ModelName")), "AirPods Pro 2 (USB-C)");
        assert_eq!(text(&value("NoiseMode")), "transparency");
        assert_eq!(text(&value("EarDetection")), "both");
        assert!(bool::try_from(value("ConversationalAwareness")).unwrap());
        assert!(bool::try_from(value("OneBudAnc")).unwrap());
        assert_eq!(u8::try_from(value("AdaptiveLevel")).unwrap(), 37);
        let batteries = BTreeMap::<String, (bool, bool, u8)>::try_from(value("Batteries")).unwrap();
        let expected_batteries = BTreeMap::from([
            ("left".to_owned(), (true, false, 80)),
            ("right".to_owned(), (true, true, 60)),
            ("case".to_owned(), (false, false, 0)),
            ("headset".to_owned(), (false, false, 0)),
        ]);
        assert_eq!(batteries, expected_batteries);
        let in_ear = BTreeMap::<String, bool>::try_from(value("InEar")).unwrap();
        let expected_in_ear =
            BTreeMap::from([("left".to_owned(), false), ("right".to_owned(), true)]);
        assert_eq!(in_ear, expected_in_ear);
        let capabilities = Vec::<String>::try_from(value("Capabilities")).unwrap();
        let expected_capabilities = [
            "adaptive",
            "conversational_awareness",
            "noise_control",
            "noise_off",
            "one_bud_anc",
        ];
        assert_eq!(capabilities, expected_capabilities);
        let status_json = text(&value("StatusJson"));
        assert_eq!(format!("{status_json}\n"), to_status_line(&populated()));
        let status: Value = serde_json::from_str(&status_json).unwrap();
        for (part, (available, charging, level)) in &expected_batteries {
            assert_eq!(status[part]["available"], *available, "{part}");
            assert_eq!(status[part]["charging"], *charging, "{part}");
            assert_eq!(status[part]["level"], *level, "{part}");
        }
        for (side, worn) in &expected_in_ear {
            assert_eq!(status[side]["in_ear"], *worn, "{side}");
        }
    });
}

#[test]
fn unknown_device_strings_are_empty_and_every_key_is_listed() {
    run(async {
        let peer = peer(Hub::new()).await;
        let all = get_all(&peer.client).await;
        let value = |name: &str| all[name].try_clone().unwrap();
        assert_eq!(text(&value("DeviceName")), "");
        assert_eq!(text(&value("ModelName")), "");
        for name in ["Connected", "ConversationalAwareness", "OneBudAnc"] {
            assert!(!bool::try_from(value(name)).unwrap(), "{name}");
        }
        let batteries = HashMap::<String, (bool, bool, u8)>::try_from(value("Batteries")).unwrap();
        let parts: BTreeSet<&str> = batteries.keys().map(String::as_str).collect();
        assert_eq!(parts, BTreeSet::from(["case", "headset", "left", "right"]));
        let in_ear = HashMap::<String, bool>::try_from(value("InEar")).unwrap();
        assert_eq!(in_ear.len(), 2);
        assert!(in_ear.contains_key("left") && in_ear.contains_key("right"));
        assert!(!in_ear.values().any(|worn| *worn));
    });
}

#[test]
fn in_ear_follows_which_bud_is_primary() {
    run(async {
        let peer = peer(Hub::new()).await;
        let left_primary = State {
            primary: Some(Component::Left),
            ..populated()
        };
        peer.hub.replace(left_primary);
        let all = get_all(&peer.client).await;
        let in_ear = BTreeMap::<String, bool>::try_from(all["InEar"].try_clone().unwrap()).unwrap();
        let expected = BTreeMap::from([("left".to_owned(), true), ("right".to_owned(), false)]);
        assert_eq!(in_ear, expected);
    });
}

#[test]
fn ear_detection_change_emits_properties_changed_once() {
    run(async {
        let peer = peer(Hub::new()).await;
        let mut stream = MessageStream::from(&peer.client);
        let ear_off = Command::SetEarDetection(EarDetectionBehavior::Disabled);
        assert_eq!(peer.hub.submit(ear_off).await, Outcome::Confirmed);
        let changed = next_change(&mut stream).await;
        assert_eq!(
            names(&changed),
            BTreeSet::from(["EarDetection", "StatusJson"])
        );
        assert_eq!(text(&changed["EarDetection"]), "off");
        // None of these changes a property, so the next signal must be the awareness change.
        assert_eq!(peer.hub.submit(ear_off).await, Outcome::Confirmed);
        let noise_off = Command::SetNoise(NoiseTarget::Mode(NoiseMode::Off));
        let refused = Outcome::Refused(CommandError::NotConnected);
        assert_eq!(peer.hub.submit(noise_off).await, refused);
        // With both buds unplaced, naming a primary changes the state and no property.
        let primary_named = State {
            primary: Some(Component::Left),
            ..peer.hub.state()
        };
        peer.hub.replace(primary_named);
        tokio::task::yield_now().await;
        let awareness_on = Command::SetConversationalAwareness(true);
        assert_eq!(peer.hub.submit(awareness_on).await, refused);
        let changed = next_change(&mut stream).await;
        assert_eq!(
            names(&changed),
            BTreeSet::from(["ConversationalAwareness", "StatusJson"])
        );
    });
}

#[test]
fn set_noise_mode_while_disconnected_returns_the_not_connected_error_name() {
    run(async {
        let peer = peer(Hub::new()).await;
        let reply = call(&peer.client, "SetNoiseMode", &("anc",)).await;
        assert_eq!(reply, Err(format!("{ERROR_PREFIX}NotConnected")));
        let refused = peer
            .client
            .call_method(
                None::<&str>,
                OBJECT_PATH,
                Some(INTERFACE),
                "SetNoiseMode",
                &("anc",),
            )
            .await;
        let Err(zbus::Error::MethodError(_, message, _)) = refused else {
            panic!("SetNoiseMode was not refused: {refused:?}");
        };
        assert_eq!(
            message.as_deref(),
            Some(CommandError::NotConnected.message())
        );
    });
}

#[test]
fn every_method_answers_with_what_submit_returns() {
    run(async {
        let peer = peer(Hub::new()).await;
        let not_connected = Err(format!("{ERROR_PREFIX}NotConnected"));
        let client = &peer.client;
        assert_eq!(
            call(client, "SetEarDetection", &("both",)).await,
            Ok("confirmed".to_owned())
        );
        assert_eq!(
            peer.hub.state().ear_detection_behavior,
            EarDetectionBehavior::PauseWhenBothRemoved
        );
        for mode in ["off", "anc", "transparency", "adaptive"] {
            assert_eq!(
                call(client, "SetNoiseMode", &(mode,)).await,
                not_connected,
                "{mode}"
            );
        }
        assert_eq!(
            call(client, "SetConversationalAwareness", &(true,)).await,
            not_connected
        );
        assert!(peer.hub.state().conversational_awareness);
        assert_eq!(call(client, "SetOneBudAnc", &(true,)).await, not_connected);
        for method in ["Connect", "Disconnect", "Forget"] {
            assert_eq!(call(client, method, &()).await, not_connected, "{method}");
        }
    });
}

#[test]
fn set_adaptive_level_accepts_0_and_100() {
    run(async {
        let peer = peer(Hub::new()).await;
        let adaptive = State {
            noise_mode: NoiseMode::Adaptive,
            ..State::default()
        };
        peer.hub.replace(adaptive);
        let not_connected = Err(format!("{ERROR_PREFIX}NotConnected"));
        for level in [0_u8, 100] {
            let reply = call(&peer.client, "SetAdaptiveLevel", &(level,)).await;
            assert_eq!(reply, not_connected, "{level}");
            assert_eq!(peer.hub.state().adaptive_noise_level.percent(), level);
        }
        let reply = call(&peer.client, "SetAdaptiveLevel", &(101_u8,)).await;
        assert_eq!(reply, Err(format!("{ERROR_PREFIX}OutOfRange")));
        assert_eq!(peer.hub.state().adaptive_noise_level.percent(), 100);
    });
}

#[test]
fn a_word_the_setting_does_not_take_is_bad_request_as_on_the_socket() {
    run(async {
        let peer = peer(Hub::new()).await;
        let bad_request = Err(format!("{ERROR_PREFIX}BadRequest"));
        for mode in ["ANC", "", "noise:anc", "anc ", "one", "true"] {
            let reply = call(&peer.client, "SetNoiseMode", &(mode,)).await;
            assert_eq!(reply, bad_request, "{mode:?}");
            assert_eq!(
                settings::setting(NOISE, mode),
                Err(CommandError::BadRequest)
            );
        }
        for behavior in ["both ", "anc", "", "ear:off", "cycle"] {
            let reply = call(&peer.client, "SetEarDetection", &(behavior,)).await;
            assert_eq!(reply, bad_request, "{behavior:?}");
        }
        assert_eq!(peer.hub.state(), State::default());
        // The socket's `set noise cycle` is a noise command too, so the method takes it.
        let reply = call(&peer.client, "SetNoiseMode", &("cycle",)).await;
        assert_eq!(reply, Err(format!("{ERROR_PREFIX}NotConnected")));
        assert_eq!(peer.hub.state().counters.noise_control_changes, 1);
    });
}

#[test]
fn introspection_xml_snapshot() {
    run(async {
        let peer = peer(Hub::new()).await;
        let xml: String = peer
            .client
            .call_method(
                None::<&str>,
                OBJECT_PATH,
                Some(INTROSPECTABLE_INTERFACE),
                "Introspect",
                &(),
            )
            .await
            .unwrap()
            .body()
            .deserialize()
            .unwrap();
        let opening = format!(r#"<interface name="{INTERFACE}">"#);
        let (_, after_opening) = xml.split_once(&opening).unwrap();
        let (body, _) = after_opening.split_once("</interface>").unwrap();
        let ours = format!("{opening}{body}</interface>");
        let squeeze = |text: &str| text.split_whitespace().collect::<String>();
        assert_eq!(squeeze(&ours), squeeze(INTROSPECTED));
    });
}

#[test]
fn every_announced_property_is_a_property_of_the_interface() {
    let announced: BTreeSet<&str> = Properties::of(&State::default())
        .values()
        .iter()
        .map(|(name, _)| *name)
        .collect();
    let introspected: BTreeSet<&str> = INTROSPECTED
        .split(r#"<property name=""#)
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .filter(|name| *name != "ApiVersion")
        .collect();
    assert_eq!(announced, introspected);
}

#[test]
fn every_mode_and_behavior_has_a_name() {
    for mode in [
        NoiseMode::Off,
        NoiseMode::Anc,
        NoiseMode::Transparency,
        NoiseMode::Adaptive,
    ] {
        assert!(!noise_mode_name(mode).is_empty(), "{mode:?}");
    }
    for behavior in [
        EarDetectionBehavior::PauseWhenOneRemoved,
        EarDetectionBehavior::PauseWhenBothRemoved,
        EarDetectionBehavior::Disabled,
    ] {
        assert!(!ear_detection_name(behavior).is_empty(), "{behavior:?}");
    }
}

#[test]
fn each_outcome_has_the_reply_the_document_names() {
    assert_eq!(answer(Outcome::Sent).ok(), Some("sent"));
    assert_eq!(answer(Outcome::Confirmed).ok(), Some("confirmed"));
    let refused = answer(Outcome::Refused(CommandError::NotWorn))
        .err()
        .unwrap();
    assert_eq!(refused.name().as_str(), format!("{ERROR_PREFIX}NotWorn"));
}

#[test]
fn every_command_error_has_its_camel_case_name() {
    for error in CommandError::ALL {
        let name = spelled(error);
        let reply = CallError::from(error);
        assert_eq!(reply.name().as_str(), name);
        assert!(ErrorName::try_from(name).is_ok(), "{name}");
        assert_eq!(reply.description(), Some(error.message()));
    }
}

#[test]
fn the_interface_document_lists_every_error_name_with_its_code() {
    for error in CommandError::ALL {
        let row = format!("| `{}` | `{}` |", spelled(error), error.code());
        assert!(API_DOCUMENT.contains(&row), "{row}");
    }
}

#[test]
fn announcing_ends_when_the_peer_goes_away() {
    run(async {
        let mut peer = peer(Hub::new()).await;
        let client = peer.client.clone();
        client.close().await.unwrap();
        (&mut peer.announcing).await.unwrap();
    });
}

#[test]
fn a_peer_that_stops_reading_neither_stalls_the_state_nor_queues_signals() {
    let (server_end, client_end) = UnixStream::pair().unwrap();
    let (built, is_built) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let stalled = thread::spawn(move || {
        let runtime = RuntimeBuilder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (client, mut stream) = runtime.block_on(async {
            let client = Builder::unix_stream(client_end)
                .p2p()
                .build()
                .await
                .unwrap();
            let stream = MessageStream::from(&client);
            (client, stream)
        });
        // Between these two calls nothing drives the client's runtime, so nothing reads what the server sends.
        built.send(()).unwrap();
        released.recv().unwrap();
        runtime.block_on(async {
            let mut before_marker = 0_usize;
            let marker = async {
                while !next_change(&mut stream)
                    .await
                    .contains_key("ConversationalAwareness")
                {
                    before_marker += 1;
                }
            };
            timeout(CEILING, marker)
                .await
                .expect("the marker never arrived");
            drop(client);
            before_marker
        })
    });
    run(async {
        let hub = Hub::new();
        let (updates, shown) = subscribe(&hub);
        let server = Builder::unix_stream(server_end)
            .server(Guid::generate())
            .p2p()
            .serve_at(OBJECT_PATH, Omapods { hub: hub.clone() })
            .build()
            .await
            .unwrap();
        is_built.recv().unwrap();
        let announcing = tokio::spawn(announce(server, updates, shown));
        let behaviors = [
            EarDetectionBehavior::Disabled,
            EarDetectionBehavior::PauseWhenOneRemoved,
        ];
        for behavior in behaviors.iter().cycle().take(FLOOD_CHANGES) {
            let submitted = hub.submit(Command::SetEarDetection(*behavior)).await;
            assert_eq!(submitted, Outcome::Confirmed);
            tokio::task::yield_now().await;
        }
        // Every submit above returned while the peer read nothing; a stalled bus holding up the runtime fails the run by `CEILING`.
        let awareness_on = Command::SetConversationalAwareness(true);
        let refused = Outcome::Refused(CommandError::NotConnected);
        assert_eq!(hub.submit(awareness_on).await, refused);
        release.send(()).unwrap();
        let joined = tokio::task::spawn_blocking(move || stalled.join());
        let before_marker = joined.await.unwrap().unwrap();
        assert!(
            before_marker < MOST_SIGNALS_HELD,
            "{before_marker} signals were held for a peer that stopped reading"
        );
        announcing.abort();
    });
}
