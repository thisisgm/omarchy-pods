use pods_proto::battery::{Batteries, Component};
use pods_proto::ear::EarReport;
use pods_proto::model::Model;
use pods_proto::noise::NoiseMode;

/// The C++ `m_adaptiveNoiseLevel` initial value.
const DEFAULT_ADAPTIVE_NOISE_LEVEL: u8 = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    /// The control channel link, not the daemon; battery keeps arriving over BLE while false.
    pub connected: bool,
    pub device_name: String,
    pub model: Model,
    /// The raw `A<NNNN>` metadata code, kept when `model` is `Unknown` or reset.
    pub model_number: String,
    pub noise_mode: NoiseMode,
    /// Percent from 0 to 100.
    pub adaptive_noise_level: u8,
    pub conversational_awareness: bool,
    pub one_bud_anc_mode: bool,
    pub batteries: Batteries,
    pub ear: EarReport,
    /// `None` before a battery report names the primary bud.
    pub primary: Option<Component>,
    pub ear_detection_behavior: EarDetectionBehavior,
    pub lid_state: LidState,
    pub counters: Counters,
}

impl Default for State {
    fn default() -> Self {
        Self {
            connected: false,
            device_name: String::new(),
            model: Model::Unknown,
            model_number: String::new(),
            noise_mode: NoiseMode::Off,
            adaptive_noise_level: DEFAULT_ADAPTIVE_NOISE_LEVEL,
            conversational_awareness: false,
            one_bud_anc_mode: false,
            batteries: Batteries::default(),
            ear: EarReport::default(),
            primary: None,
            ear_detection_behavior: EarDetectionBehavior::default(),
            lid_state: LidState::default(),
            counters: Counters::default(),
        }
    }
}

impl State {
    /// Forgets what the pods reported, as the C++ `DeviceInfo::reset` does; preferences, the model number and counters stay.
    pub fn reset_device(&mut self) {
        self.connected = false;
        self.device_name.clear();
        self.model = Model::Unknown;
        self.noise_mode = NoiseMode::Off;
        self.batteries = Batteries::default();
        self.ear = EarReport::default();
        self.primary = None;
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
