//! The daemon's D-Bus interface on the session bus: properties that mirror the state and methods that submit commands.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::future::{Future, poll_fn};
use std::pin::{Pin, pin};
use std::task::Poll;

use pods_core::command::{ADAPTIVE_LEVEL_MAX, Command, CommandError, NoiseTarget, Outcome};
use pods_core::legacy::{LegacyRequest, parse_verb, verb_for};
use pods_core::state::{EarDetectionBehavior, State};
use pods_core::status::{in_ear, to_status_object};
use pods_proto::battery::{Charge, Component};
use pods_proto::noise::NoiseMode;
use serde_json::Value;
use tokio::sync::watch;
use zbus::connection::Builder;
use zbus::names::InterfaceName;
use zbus::object_server::SignalEmitter;
use zbus::{Connection, Variant, fdo, interface};

use super::hub::Hub;
use super::log;

/// The well-known name the daemon owns on the session bus.
const BUS_NAME: &str = "io.github.thisisgm.Omapods";

/// The one object the daemon serves.
const OBJECT_PATH: &str = "/io/github/thisisgm/Omapods";

/// The interface name; its trailing number is the interface version.
const INTERFACE: &str = "io.github.thisisgm.Omapods1";

/// The value of `ApiVersion`, the number in the interface name.
const API_VERSION: u32 = 1;

/// Every line the daemon logs about this interface starts with this.
const LOG_PREFIX: &str = "D-Bus interface off";

/// The legacy verb family whose values name the listening modes, such as `noise:anc`.
const NOISE_VERB_PREFIX: &str = "noise:";

/// The legacy verb family whose values name the ear detection behaviours, such as `ear:both`.
const EAR_VERB_PREFIX: &str = "ear:";

/// The status document key prefix of the flags `Capabilities` lists.
const CAPABILITY_KEY_PREFIX: &str = "supports_";

/// What a method returns when the pods have the command but have not answered.
const SENT: &str = "sent";

/// What a method returns when the state already shows the command carried out.
const CONFIRMED: &str = "confirmed";

/// Serves the interface for as long as the session bus keeps the connection, or logs one line and returns at once.
pub(super) async fn serve(hub: Hub) {
    // Taken before the object is served, so a change while the name is requested is still announced.
    let updates = hub.subscribe();
    let shown = Properties::of(&updates.borrow());
    // zbus would otherwise take the name from a running daemon and let the next one take it back.
    let connection = Builder::session()
        .name(BUS_NAME)
        .replace_existing_names(false)
        .allow_name_replacements(false)
        .serve_at(OBJECT_PATH, Omapods { hub: hub.clone() })
        .build()
        .await;
    match connection {
        Ok(connection) => announce(connection, updates, shown).await,
        Err(error) => log(&format!(
            "{LOG_PREFIX}: cannot own {BUS_NAME} on the session bus: {error}"
        )),
    }
}

/// Sends one `PropertiesChanged` per state change with the properties that differ from `shown`, until the bus goes away.
async fn announce(
    connection: Connection,
    mut updates: watch::Receiver<State>,
    mut shown: Properties,
) {
    let (emitter, interface) = match SignalEmitter::new(&connection, OBJECT_PATH)
        .and_then(|emitter| Ok((emitter, InterfaceName::from_static_str(INTERFACE)?)))
    {
        Ok(names) => names,
        Err(error) => {
            log(&format!(
                "{LOG_PREFIX}: {OBJECT_PATH} or {INTERFACE} is not a valid D-Bus name: {error}"
            ));
            return;
        }
    };
    let mut closed = pin!(connection.closed());
    loop {
        match first_of(closed.as_mut(), updates.changed()).await {
            Wake::BusClosed => {
                log(&format!(
                    "{LOG_PREFIX}: the session bus closed the connection"
                ));
                return;
            }
            // The hub keeps the sender for the life of the daemon, so a closed channel means the runtime is stopping.
            Wake::StateChanged(Err(_)) => return,
            Wake::StateChanged(Ok(())) => {}
        }
        let now = Properties::of(&updates.borrow_and_update());
        let changed = now.changed_since(&shown);
        if !changed.is_empty() {
            let sent = fdo::Properties::properties_changed(
                &emitter,
                interface.clone(),
                changed,
                Cow::Borrowed(&[]),
            )
            .await;
            if let Err(error) = sent {
                log(&format!(
                    "{LOG_PREFIX}: cannot announce a property change: {error}"
                ));
                return;
            }
        }
        shown = now;
    }
}

/// Why the announcing loop woke.
enum Wake<T> {
    BusClosed,
    StateChanged(T),
}

/// Waits for the bus to close or `changed` to finish; a closed bus wins a tie, so nothing is written to it.
async fn first_of<F: Future>(
    mut closed: Pin<&mut impl Future<Output = ()>>,
    changed: F,
) -> Wake<F::Output> {
    let mut changed = pin!(changed);
    poll_fn(|context| {
        if closed.as_mut().poll(context).is_ready() {
            return Poll::Ready(Wake::BusClosed);
        }
        changed.as_mut().poll(context).map(Wake::StateChanged)
    })
    .await
}

/// The interface's properties as one state shows them, all but the constant `ApiVersion`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Properties {
    connected: bool,
    device_name: String,
    model_name: &'static str,
    noise_mode: &'static str,
    ear_detection: &'static str,
    conversational_awareness: bool,
    one_bud_anc: bool,
    adaptive_level: u8,
    batteries: BTreeMap<&'static str, (bool, bool, u8)>,
    in_ear: BTreeMap<&'static str, bool>,
    capabilities: Vec<String>,
    status_json: String,
}

impl Properties {
    fn of(state: &State) -> Self {
        let status = to_status_object(state);
        let mut capabilities: Vec<String> = status
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(_, value)| **value == Value::Bool(true))
            .filter_map(|(key, _)| key.strip_prefix(CAPABILITY_KEY_PREFIX))
            .map(str::to_owned)
            .collect();
        capabilities.sort();
        let batteries = &state.batteries;
        Self {
            connected: state.connected,
            device_name: state.device_name.clone(),
            model_name: state.model.display_name(),
            noise_mode: noise_mode_name(state.noise_mode),
            ear_detection: ear_detection_name(state.ear_detection_behavior),
            conversational_awareness: state.conversational_awareness,
            one_bud_anc: state.one_bud_anc_mode,
            adaptive_level: state.adaptive_noise_level,
            batteries: BTreeMap::from([
                ("left", battery(batteries.left)),
                ("right", battery(batteries.right)),
                ("case", battery(batteries.case)),
                ("headset", battery(batteries.headset)),
            ]),
            in_ear: BTreeMap::from([
                ("left", in_ear(state, Component::Left)),
                ("right", in_ear(state, Component::Right)),
            ]),
            capabilities,
            status_json: status.to_string(),
        }
    }

    /// Each property by its D-Bus name, in the order of the struct.
    fn values(&self) -> [(&'static str, Variant<'_>); 12] {
        [
            ("Connected", Variant::from(self.connected)),
            ("DeviceName", Variant::from(self.device_name.as_str())),
            ("ModelName", Variant::from(self.model_name)),
            ("NoiseMode", Variant::from(self.noise_mode)),
            ("EarDetection", Variant::from(self.ear_detection)),
            (
                "ConversationalAwareness",
                Variant::from(self.conversational_awareness),
            ),
            ("OneBudAnc", Variant::from(self.one_bud_anc)),
            ("AdaptiveLevel", Variant::from(self.adaptive_level)),
            ("Batteries", Variant::from(self.batteries.clone())),
            ("InEar", Variant::from(self.in_ear.clone())),
            ("Capabilities", Variant::from(self.capabilities.clone())),
            ("StatusJson", Variant::from(self.status_json.as_str())),
        ]
    }

    /// The properties whose value differs from `before`, by D-Bus name.
    fn changed_since(&self, before: &Self) -> HashMap<&'static str, Variant<'_>> {
        self.values()
            .into_iter()
            .zip(before.values())
            .filter(|((_, now), (_, was))| now != was)
            .map(|(change, _)| change)
            .collect()
    }
}

/// A part with no reading shows as not available, not charging, level 0, as the status document shows it.
fn battery(charge: Option<Charge>) -> (bool, bool, u8) {
    charge.map_or((false, false, 0), |charge| {
        (true, charge.charging, charge.level)
    })
}

/// The value of the `noise:` verb that selects `mode`, such as `anc`.
fn noise_mode_name(mode: NoiseMode) -> &'static str {
    let request = LegacyRequest::Command(Command::SetNoise(NoiseTarget::Mode(mode)));
    verb_value(request, NOISE_VERB_PREFIX)
}

/// The value of the `ear:` verb that selects `behavior`, such as `both`.
fn ear_detection_name(behavior: EarDetectionBehavior) -> &'static str {
    verb_value(
        LegacyRequest::Command(Command::SetEarDetection(behavior)),
        EAR_VERB_PREFIX,
    )
}

/// What follows `prefix` in the verb for `request`; a test pins that every mode and behaviour has one.
fn verb_value(request: LegacyRequest, prefix: &str) -> &'static str {
    verb_for(request)
        .and_then(|verb| verb.strip_prefix(prefix))
        .unwrap_or_default()
}

/// The command a method's string argument names under `prefix`, refusing anything that is not `wanted`.
// Sample input: `anc` under `noise:`, `both` under `ear:`.
fn command_named(
    prefix: &str,
    value: &str,
    wanted: fn(Command) -> bool,
) -> Result<Command, CallError> {
    match parse_verb(format!("{prefix}{value}").as_bytes()) {
        Some(LegacyRequest::Command(command)) if wanted(command) => Ok(command),
        Some(_) | None => Err(CommandError::UnknownKey.into()),
    }
}

/// The object at `OBJECT_PATH`: properties read the hub's state, and methods submit commands to it.
#[derive(Debug)]
struct Omapods {
    hub: Hub,
}

impl Omapods {
    fn now(&self) -> Properties {
        Properties::of(&self.hub.state())
    }

    async fn submit(&self, command: Command) -> Result<&'static str, CallError> {
        match self.hub.submit(command).await {
            Outcome::Sent => Ok(SENT),
            Outcome::Confirmed => Ok(CONFIRMED),
            Outcome::Refused(error) => Err(error.into()),
        }
    }
}

#[interface(name = "io.github.thisisgm.Omapods1", introspection_docs = false)]
impl Omapods {
    #[zbus(property(emits_changed_signal = "const"))]
    #[expect(
        clippy::unused_self,
        reason = "the interface macro serves a property only from a method"
    )]
    fn api_version(&self) -> u32 {
        API_VERSION
    }

    #[zbus(property)]
    fn connected(&self) -> bool {
        self.now().connected
    }

    #[zbus(property)]
    fn device_name(&self) -> String {
        self.now().device_name
    }

    #[zbus(property)]
    fn model_name(&self) -> &'static str {
        self.now().model_name
    }

    #[zbus(property)]
    fn noise_mode(&self) -> &'static str {
        self.now().noise_mode
    }

    #[zbus(property)]
    fn ear_detection(&self) -> &'static str {
        self.now().ear_detection
    }

    #[zbus(property)]
    fn conversational_awareness(&self) -> bool {
        self.now().conversational_awareness
    }

    #[zbus(property)]
    fn one_bud_anc(&self) -> bool {
        self.now().one_bud_anc
    }

    #[zbus(property)]
    fn adaptive_level(&self) -> u8 {
        self.now().adaptive_level
    }

    #[zbus(property)]
    fn batteries(&self) -> BTreeMap<&'static str, (bool, bool, u8)> {
        self.now().batteries
    }

    #[zbus(property)]
    fn in_ear(&self) -> BTreeMap<&'static str, bool> {
        self.now().in_ear
    }

    #[zbus(property)]
    fn capabilities(&self) -> Vec<String> {
        self.now().capabilities
    }

    #[zbus(property)]
    fn status_json(&self) -> String {
        self.now().status_json
    }

    async fn set_noise_mode(&self, mode: &str) -> Result<&'static str, CallError> {
        let is_mode = |command| matches!(command, Command::SetNoise(NoiseTarget::Mode(_)));
        self.submit(command_named(NOISE_VERB_PREFIX, mode, is_mode)?)
            .await
    }

    async fn set_ear_detection(&self, behavior: &str) -> Result<&'static str, CallError> {
        let is_behavior = |command| matches!(command, Command::SetEarDetection(_));
        self.submit(command_named(EAR_VERB_PREFIX, behavior, is_behavior)?)
            .await
    }

    async fn set_conversational_awareness(&self, enabled: bool) -> Result<&'static str, CallError> {
        self.submit(Command::SetConversationalAwareness(enabled))
            .await
    }

    async fn set_one_bud_anc(&self, enabled: bool) -> Result<&'static str, CallError> {
        self.submit(Command::SetOneBudAnc(enabled)).await
    }

    async fn set_adaptive_level(&self, level: u8) -> Result<&'static str, CallError> {
        if level > ADAPTIVE_LEVEL_MAX {
            return Err(CommandError::OutOfRange.into());
        }
        self.submit(Command::SetAdaptiveLevel(level)).await
    }

    async fn connect(&self) -> Result<&'static str, CallError> {
        self.submit(Command::Connect).await
    }

    async fn disconnect(&self) -> Result<&'static str, CallError> {
        self.submit(Command::Disconnect).await
    }

    async fn forget(&self) -> Result<&'static str, CallError> {
        self.submit(Command::Forget).await
    }
}

/// A refused command as a D-Bus error reply, named after its `CommandError` variant, carrying the error's code.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "io.github.thisisgm.Omapods1.Error")]
enum CallError {
    NotConnected(String),
    NotSupported(String),
    NotWorn(String),
    OutOfRange(String),
    UnknownKey(String),
    BadRequest(String),
    Timeout(String),
    Headless(String),
}

impl From<CommandError> for CallError {
    fn from(error: CommandError) -> Self {
        let code = error.code().to_owned();
        match error {
            CommandError::NotConnected => Self::NotConnected(code),
            CommandError::NotSupported => Self::NotSupported(code),
            CommandError::NotWorn => Self::NotWorn(code),
            CommandError::OutOfRange => Self::OutOfRange(code),
            CommandError::UnknownKey => Self::UnknownKey(code),
            CommandError::BadRequest => Self::BadRequest(code),
            CommandError::Timeout => Self::Timeout(code),
            CommandError::Headless => Self::Headless(code),
        }
    }
}

#[cfg(test)]
mod tests;
