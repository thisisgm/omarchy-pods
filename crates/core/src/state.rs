use pods_proto::battery::{Batteries, Component};
use pods_proto::ear::EarReport;
use pods_proto::model::{Capabilities, Model};
use pods_proto::noise::NoiseMode;

use crate::command::AdaptiveLevel;
use crate::prefs::{MicModels, MicPref};

#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is its own key of the status document, a JSON bool"
)]
pub struct State {
    /// The control channel link, not the daemon; battery keeps arriving over BLE while false.
    pub connected: bool,
    pub device_name: String,
    pub model: Model,
    /// The raw `A<NNNN>` metadata code, kept when it names no known model; a reset clears it.
    pub model_number: String,
    pub noise_mode: NoiseMode,
    /// Whether the pods reported they offer Off; `None` until they do.
    pub noise_off: Option<bool>,
    pub adaptive_noise_level: AdaptiveLevel,
    pub conversational_awareness: bool,
    pub one_bud_anc_mode: bool,
    pub batteries: Batteries,
    pub ear: EarReport,
    /// `None` before a battery report names the primary bud.
    pub primary: Option<Component>,
    pub ear_detection_behavior: EarDetectionBehavior,
    pub lid_state: LidState,
    pub counters: Counters,
    /// Whether the policy sends desktop notices.
    pub notifications: bool,
    pub microphone: Microphone,
    /// The models whose uplink answered, which `auto` does not start again.
    pub mic_models: MicModels,
    pub audio_server: AudioServer,
    pub advert_intake: AdvertIntake,
}

impl Default for State {
    fn default() -> Self {
        Self {
            connected: false,
            device_name: String::new(),
            model: Model::Unknown,
            model_number: String::new(),
            noise_mode: NoiseMode::Off,
            noise_off: None,
            adaptive_noise_level: AdaptiveLevel::default(),
            conversational_awareness: false,
            one_bud_anc_mode: false,
            batteries: Batteries::default(),
            ear: EarReport::default(),
            primary: None,
            ear_detection_behavior: EarDetectionBehavior::default(),
            lid_state: LidState::default(),
            counters: Counters::default(),
            // The C++ `Notifier` starts enabled.
            notifications: true,
            microphone: Microphone::default(),
            mic_models: MicModels::default(),
            audio_server: AudioServer::Idle,
            advert_intake: AdvertIntake::Paused,
        }
    }
}

const MIC_NODE_PREFIX: &str = "omarchypods_input.";

impl State {
    /// Forgets what the pods reported; preferences and counters stay.
    pub fn reset_device(&mut self) {
        self.device_name.clear();
        self.model_number.clear();
        self.model = Model::Unknown;
        self.noise_mode = NoiseMode::Off;
        self.noise_off = None;
        self.batteries = Batteries::default();
        self.ear = EarReport::default();
        self.primary = None;
    }

    /// The pods' own report when they sent one, else the model table.
    #[must_use]
    pub fn supports_noise_off(&self) -> bool {
        self.noise_off
            .unwrap_or_else(|| self.model.capabilities().noise_off)
    }

    /// The source node's name, the model number in lower case so each model keeps its own.
    #[must_use]
    pub fn mic_node_name(&self) -> String {
        format!("{MIC_NODE_PREFIX}{}", self.model_number.to_lowercase())
    }

    /// The node's name while it is on the server, which `mic_source` shows; empty otherwise.
    #[must_use]
    pub fn mic_source(&self) -> String {
        if self.microphone.source_up() {
            self.mic_node_name()
        } else {
            String::new()
        }
    }

    #[must_use]
    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            noise_off: self.supports_noise_off(),
            ..self.model.capabilities()
        }
    }
}

/// When taking a bud out pauses playback, numbered as `MediaController::EarDetectionBehavior`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EarDetectionBehavior {
    #[default]
    PauseWhenOneRemoved,
    PauseWhenBothRemoved,
    Disabled,
}

impl EarDetectionBehavior {
    pub const ALL: [Self; 3] = [
        Self::PauseWhenOneRemoved,
        Self::PauseWhenBothRemoved,
        Self::Disabled,
    ];

    /// The word after `ear:` in a legacy verb, which `set ear_detection` takes and the `EarDetection` property shows.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::PauseWhenOneRemoved => "one",
            Self::PauseWhenBothRemoved => "both",
            Self::Disabled => "off",
        }
    }

    pub(crate) fn number(self) -> u8 {
        match self {
            Self::PauseWhenOneRemoved => 0,
            Self::PauseWhenBothRemoved => 1,
            Self::Disabled => 2,
        }
    }
}

/// The case lid as the proximity advertisement reports it, numbered as `BleInfo::LidState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LidState {
    Open,
    Closed,
    #[default]
    Unknown,
}

impl LidState {
    pub(crate) fn number(self) -> u8 {
        match self {
            Self::Open => 0,
            Self::Closed => 1,
            Self::Unknown => 2,
        }
    }
}

/// The microphone preference and what the `mic` status keys show, held together so neither moves without the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Microphone {
    preference: MicPref,
    code: MicCode,
    start_failures: u32,
    /// Whether the source node is on the audio server.
    source_up: bool,
}

impl Microphone {
    #[must_use]
    pub const fn new(preference: MicPref) -> Self {
        Self {
            preference,
            code: MicCode::resting(preference),
            start_failures: 0,
            source_up: false,
        }
    }

    #[must_use]
    pub const fn preference(self) -> MicPref {
        self.preference
    }

    #[must_use]
    pub const fn code(self) -> MicCode {
        self.code
    }

    #[must_use]
    pub const fn start_failures(self) -> u32 {
        self.start_failures
    }

    #[must_use]
    pub const fn source_up(self) -> bool {
        self.source_up
    }

    pub fn set_preference(&mut self, preference: MicPref) {
        self.preference = preference;
        self.code = MicCode::resting(preference);
    }

    /// Shows the uplink machine's `code` and `start_failures` and whether the node is up, except that `off` is shown exactly while the preference is `off`.
    pub fn show(&mut self, code: MicCode, start_failures: u32, source_up: bool) {
        self.start_failures = start_failures;
        self.source_up = source_up;
        self.code = match (self.preference, code) {
            (MicPref::Off, _) | (_, MicCode::Off) => MicCode::resting(self.preference),
            (MicPref::Auto | MicPref::Always, code) => code,
        };
    }
}

impl Default for Microphone {
    fn default() -> Self {
        Self::new(MicPref::default())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicCode {
    Off,
    Idle,
    NotWorn,
    Unsupported,
    Starting,
    Live,
    Stopping,
    Failed,
    Ready,
    Recording,
    NoAudioServer,
    LibraryError,
}

impl MicCode {
    const fn resting(preference: MicPref) -> Self {
        match preference {
            MicPref::Off => Self::Off,
            MicPref::Auto | MicPref::Always => Self::Idle,
        }
    }

    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Idle => "idle",
            Self::NotWorn => "not_worn",
            Self::Unsupported => "unsupported",
            Self::Starting => "starting",
            Self::Live => "live",
            Self::Stopping => "stopping",
            Self::Failed => "failed",
            Self::Ready => "ready",
            Self::Recording => "recording",
            Self::NoAudioServer => "no_audio_server",
            Self::LibraryError => "library_error",
        }
    }
}

/// The daemon's connection to the desktop's audio server, which it holds only while a device's link is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioServer {
    Idle,
    Up,
    Down,
}

impl AudioServer {
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Up => "up",
            Self::Down => "down",
        }
    }
}

/// Whether the daemon reads the pods' adverts through the BlueZ advert monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdvertIntake {
    /// BlueZ accepted the monitor.
    Listening,
    /// Neither of the others: the link is up, no keys are held, or BlueZ has not accepted the registration yet.
    Paused,
    /// While the monitor is wanted (keys held, no link up) and not accepted because no adapter offered it or each registration was refused.
    Unavailable,
}

impl AdvertIntake {
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Listening => "listening",
            Self::Paused => "paused",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    pub reconnect_attempts: u64,
    pub reconnect_failures: u64,
    pub noise_control_changes: u64,
    pub forget_calls: u64,
    pub ear_detection_changes: u64,
    /// Published as `ca_changes_total`.
    pub conversational_awareness_changes: u64,
    pub disconnect_calls: u64,
    pub connect_calls: u64,
    pub disconnect_failures: u64,
    pub connect_failures: u64,
    pub adaptive_level_changes: u64,
    pub one_bud_anc_changes: u64,
    pub reopen_calls: u64,
}

#[cfg(test)]
mod tests;
