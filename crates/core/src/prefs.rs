//! The choices kept across restarts, their file and the C++ daemon's old settings file.

use std::fmt;

use pods_proto::model::canonical_model_number;
use serde_json::{Value, json};

use crate::state::{EarDetectionBehavior, Microphone, State};

pub const VERSION: u32 = 1;

/// More models than Apple sells.
pub const MAX_CONFIRMED_MODELS: usize = 16;

/// A model number is a letter and four digits today; eight bounds the file with room.
pub const MODEL_NUMBER_MAX: usize = 8;

const VERSION_KEY: &str = "version";

const EAR_DETECTION_KEY: &str = "ear_detection";

const NOTIFICATIONS_KEY: &str = "notifications";

const MICROPHONE_KEY: &str = "microphone";

const MIC_MODELS_KEY: &str = "mic_models";

/// The legacy ear-detection preference, stored as `earDetection/setting`.
const OLD_EAR_DETECTION: (&str, &str) = ("earDetection", "setting");

/// The legacy notification preference, stored as `notifications/enabled`.
const OLD_NOTIFICATIONS: (&str, &str) = ("notifications", "enabled");

/// Whether the daemon may use the pods' microphone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MicPref {
    #[default]
    Auto,
    Always,
    Off,
}

impl MicPref {
    pub const ALL: [Self; 3] = [Self::Auto, Self::Always, Self::Off];

    /// The word `set microphone` takes, the settings file keeps and the `microphone` status key shows.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Off => "off",
        }
    }

    // Sample input: `always`.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|preference| preference.word() == word)
    }
}

/// The model numbers whose pods answered a START, oldest first, none twice.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MicModels(Vec<String>);

impl MicModels {
    #[must_use]
    pub fn contains(&self, number: &str) -> bool {
        let number = canonical_model_number(number);
        self.0
            .iter()
            .any(|kept| canonical_model_number(kept) == number)
    }

    /// Keeps `number` unless it is kept already or is no model number, dropping the oldest past `MAX_CONFIRMED_MODELS`.
    pub fn confirm(&mut self, number: &str) {
        if self.contains(number) || !is_model_number(number) {
            return;
        }
        if self.0.len() == MAX_CONFIRMED_MODELS {
            self.0.remove(0);
        }
        self.0.push(canonical_model_number(number).to_owned());
    }

    // Sample input: `["A3063","A2084"]`.
    fn parse(value: &Value) -> Option<Self> {
        let entries = value.as_array()?;
        let mut models = Self::default();
        for entry in entries {
            let number = entry.as_str().filter(|number| is_model_number(number))?;
            // Older files can contain distinct numbers for the same model.
            if models.0.iter().any(|kept| kept == number) || models.0.len() == MAX_CONFIRMED_MODELS
            {
                return None;
            }
            models.0.push(number.to_owned());
        }
        Some(models)
    }
}

/// One to `MODEL_NUMBER_MAX` characters of `A` to `Z` and `0` to `9`.
fn is_model_number(text: &str) -> bool {
    (1..=MODEL_NUMBER_MAX).contains(&text.len())
        && text
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prefs {
    pub ear_detection: EarDetectionBehavior,
    pub notifications: bool,
    pub microphone: MicPref,
    pub mic_models: MicModels,
}

impl Default for Prefs {
    fn default() -> Self {
        Self::of(&State::default())
    }
}

impl Prefs {
    #[must_use]
    pub fn of(state: &State) -> Self {
        Self {
            ear_detection: state.ear_detection_behavior,
            notifications: state.notifications,
            microphone: state.microphone.preference(),
            mic_models: state.mic_models.clone(),
        }
    }

    /// A fresh state that carries these choices.
    #[must_use]
    pub fn into_state(self) -> State {
        State {
            ear_detection_behavior: self.ear_detection,
            notifications: self.notifications,
            microphone: Microphone::new(self.microphone),
            mic_models: self.mic_models,
            ..State::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefsError {
    NotAnObject,
    Version,
    EarDetection,
    Notifications,
    Microphone,
    MicModels,
}

impl fmt::Display for PrefsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NotAnObject => "not a JSON object",
            Self::Version => "no version 1",
            Self::EarDetection => "no ear_detection word",
            Self::Notifications => "no notifications switch",
            Self::Microphone => "no microphone word",
            Self::MicModels => "no mic_models list",
        })
    }
}

// Sample input: `{"version":1,"ear_detection":"one","notifications":true,"microphone":"auto","mic_models":["A3063"]}`; a file may lack the last two.
pub fn parse(text: &str) -> Result<Prefs, PrefsError> {
    let document: Option<Value> = serde_json::from_str(text).ok();
    let object = document
        .as_ref()
        .and_then(Value::as_object)
        .ok_or(PrefsError::NotAnObject)?;
    if object.get(VERSION_KEY).and_then(Value::as_u64) != Some(u64::from(VERSION)) {
        return Err(PrefsError::Version);
    }
    let ear_detection = object
        .get(EAR_DETECTION_KEY)
        .and_then(Value::as_str)
        .and_then(|word| {
            EarDetectionBehavior::ALL
                .into_iter()
                .find(|behavior| behavior.word() == word)
        })
        .ok_or(PrefsError::EarDetection)?;
    let notifications = object
        .get(NOTIFICATIONS_KEY)
        .and_then(Value::as_bool)
        .ok_or(PrefsError::Notifications)?;
    let microphone = match object.get(MICROPHONE_KEY) {
        Some(word) => word
            .as_str()
            .and_then(MicPref::from_word)
            .ok_or(PrefsError::Microphone)?,
        // A file written before the daemon kept this choice.
        None => MicPref::default(),
    };
    let mic_models = match object.get(MIC_MODELS_KEY) {
        Some(list) => MicModels::parse(list).ok_or(PrefsError::MicModels)?,
        None => MicModels::default(),
    };
    Ok(Prefs {
        ear_detection,
        notifications,
        microphone,
        mic_models,
    })
}

/// One line of JSON with its newline.
#[must_use]
pub fn render(prefs: &Prefs) -> String {
    let document = json!({
        VERSION_KEY: VERSION,
        EAR_DETECTION_KEY: prefs.ear_detection.word(),
        NOTIFICATIONS_KEY: prefs.notifications,
        MICROPHONE_KEY: prefs.microphone.word(),
        MIC_MODELS_KEY: prefs.mic_models.0,
    });
    format!("{document}\n")
}

// Sample input: `[earDetection]` / `setting=1` / `[notifications]` / `enabled=false`; anything unread is the default.
#[must_use]
pub fn from_old_conf(text: &str) -> Prefs {
    let mut prefs = Prefs::default();
    let mut group = "";
    for line in text.lines().map(str::trim) {
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            group = name;
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let at = (group, key.trim());
        let value = value.trim();
        if at == OLD_EAR_DETECTION {
            if let Some(behavior) = value.parse().ok().and_then(behavior_numbered) {
                prefs.ear_detection = behavior;
            }
        } else if at == OLD_NOTIFICATIONS
            && let Ok(enabled) = value.parse()
        {
            prefs.notifications = enabled;
        }
    }
    prefs
}

/// The C++ `MediaController::EarDetectionBehavior` number, as the old file stores it.
fn behavior_numbered(number: u8) -> Option<EarDetectionBehavior> {
    EarDetectionBehavior::ALL
        .into_iter()
        .find(|behavior| behavior.number() == number)
}

#[cfg(test)]
mod tests;
