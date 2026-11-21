//! The daemon's D-Bus interface on the session bus: properties that mirror the state and methods that submit commands.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::future::{Future, poll_fn};
use std::num::NonZeroUsize;
use std::pin::{Pin, pin};
use std::task::Poll;

use pods_core::command::{Command, CommandError, NoiseTarget, Outcome};
use pods_core::settings::{
    self, ADAPTIVE_LEVEL, CONVERSATIONAL_AWARENESS, EAR_DETECTION, NOISE, ONE_BUD_ANC,
};
use pods_core::state::{EarDetectionBehavior, State};
use pods_core::status::{battery_reading, in_ear, to_status_object};
use pods_proto::battery::Component;
use pods_proto::noise::NoiseMode;
use serde_json::Value;
use tokio::sync::watch;
use zbus::connection::Builder;
use zbus::message::Header;
use zbus::names::ErrorName;
use zbus::object_server::{Interface, SignalEmitter};
use zbus::{Connection, DBusError, Message, Variant, fdo, interface};

use super::hub::Hub;
use super::log;

/// The well-known name the daemon owns on the session bus.
const BUS_NAME: &str = "io.github.thisisgm.Omapods";

/// The one object the daemon serves.
const OBJECT_PATH: &str = "/io/github/thisisgm/Omapods";

/// The name segment between the interface name and an error's own name.
const ERROR_SEGMENT: &str = "Error";

/// Messages the connection queues for the object server before it stops reading the bus, zbus's own default named here.
const MAX_QUEUED_MESSAGES: NonZeroUsize = match NonZeroUsize::new(64) {
    Some(bound) => bound,
    None => NonZeroUsize::MIN,
};

/// The value of `ApiVersion`, the number in the interface name.
const API_VERSION: u32 = 1;

/// Every line the daemon logs about this interface starts with this.
const LOG_PREFIX: &str = "D-Bus interface off";

/// The status document key prefix of the flags `Capabilities` lists.
const CAPABILITY_KEY_PREFIX: &str = "supports_";

/// Serves the interface for as long as the session bus keeps the connection, or logs one line and returns at once.
pub(super) async fn serve(hub: Hub) {
    // A watch, not `follow`: a property signal carries values, so a slow bus gets the net change, never a backlog or a drop.
    let updates = hub.subscribe();
    // Taken before the object is served, so a change while the name is requested is still announced.
    let shown = Properties::of(&updates.borrow());
    // zbus would otherwise take the name from a running daemon and let the next one take it back.
    let connection = Builder::session()
        .name(BUS_NAME)
        .replace_existing_names(false)
        .allow_name_replacements(false)
        .max_queued(MAX_QUEUED_MESSAGES)
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

/// Each time it wakes, reads only the latest state and announces the properties that differ from the last signal, until the bus goes away.
async fn announce(
    connection: Connection,
    mut updates: watch::Receiver<State>,
    mut shown: Properties,
) {
    let emitter = match SignalEmitter::new(&connection, OBJECT_PATH) {
        Ok(emitter) => emitter,
        Err(error) => {
            log(&format!(
                "{LOG_PREFIX}: {OBJECT_PATH} is not a valid object path: {error}"
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
                Omapods::name(),
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
            adaptive_level: state.adaptive_noise_level.percent(),
            batteries: BTreeMap::from([
                ("left", battery_reading(batteries.left)),
                ("right", battery_reading(batteries.right)),
                ("case", battery_reading(batteries.case)),
                ("headset", battery_reading(batteries.headset)),
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

/// The word `set noise` takes for `mode`, such as `anc`.
fn noise_mode_name(mode: NoiseMode) -> &'static str {
    let command = Command::SetNoise(NoiseTarget::Mode(mode));
    settings::word(NOISE, command).unwrap_or_default()
}

/// The word `set ear_detection` takes for `behavior`, such as `both`; a test pins that every behaviour and mode has one.
fn ear_detection_name(behavior: EarDetectionBehavior) -> &'static str {
    settings::word(EAR_DETECTION, Command::SetEarDetection(behavior)).unwrap_or_default()
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

    /// Submits what `set <key> <value>` asks for on the control socket, by the same rules.
    async fn set(&self, key: &str, value: &str) -> Result<&'static str, CallError> {
        self.submit(settings::setting(key, value)?).await
    }

    async fn submit(&self, command: Command) -> Result<&'static str, CallError> {
        answer(self.hub.submit(command).await)
    }
}

/// A method's reply for how its command ended.
fn answer(outcome: Outcome) -> Result<&'static str, CallError> {
    Ok(outcome.name()?)
}

// The interface name is written only here, and its trailing number is the interface version.
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
        self.set(NOISE, mode).await
    }

    async fn set_ear_detection(&self, behavior: &str) -> Result<&'static str, CallError> {
        self.set(EAR_DETECTION, behavior).await
    }

    async fn set_conversational_awareness(&self, enabled: bool) -> Result<&'static str, CallError> {
        self.set(CONVERSATIONAL_AWARENESS, &enabled.to_string())
            .await
    }

    async fn set_one_bud_anc(&self, enabled: bool) -> Result<&'static str, CallError> {
        self.set(ONE_BUD_ANC, &enabled.to_string()).await
    }

    async fn set_adaptive_level(&self, level: u8) -> Result<&'static str, CallError> {
        self.set(ADAPTIVE_LEVEL, &level.to_string()).await
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

/// A refused command as a D-Bus error reply: named after its `CommandError` variant, carrying the error's sentence.
#[derive(Debug)]
struct CallError(CommandError);

impl From<CommandError> for CallError {
    fn from(error: CommandError) -> Self {
        Self(error)
    }
}

impl DBusError for CallError {
    fn create_reply(&self, call: &Header<'_>) -> zbus::Result<Message> {
        // The name is checked here, so a name that broke the D-Bus rules would fail the reply, never send it.
        Message::error(call, error_name(self.0.code())).build(&(self.0.message(),))
    }

    fn name(&self) -> ErrorName<'_> {
        // A test pins every name against the D-Bus name rules, which the snake case codes always meet.
        ErrorName::from_string_unchecked(error_name(self.0.code()))
    }

    fn description(&self) -> Option<&str> {
        Some(self.0.message())
    }
}

/// The error name of a refusal: the interface name, `Error` and the code in CamelCase.
// Sample input: `not_connected`, which names `io.github.thisisgm.Omapods1.Error.NotConnected`.
fn error_name(code: &str) -> String {
    let mut name = format!("{}.{ERROR_SEGMENT}.", Omapods::name());
    for word in code.split('_') {
        let mut letters = word.chars();
        if let Some(first) = letters.next() {
            name.push(first.to_ascii_uppercase());
            name.extend(letters);
        }
    }
    name
}

#[cfg(test)]
mod tests;
