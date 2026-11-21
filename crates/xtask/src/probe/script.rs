//! The timed scripts a capture follows: what to send and what to tell the owner, at offsets from the gate.

use std::fmt;
use std::str::FromStr;

use pods_proto::uplink::MicSide;

/// Milliseconds in the minutes `--minutes` counts.
const MS_PER_MINUTE: u32 = 60_000;
/// The shortest run `--minutes` asks for.
const MIN_MINUTES: u32 = 1;
/// The longest run `--minutes` asks for.
const MAX_MINUTES: u32 = 90;
/// How long `knock` holds the channel open after the connect.
const KNOCK_HOLD_MS: u32 = 3_000;
/// How long `soak` keeps logging after its STOP.
const SOAK_TAIL_MS: u32 = 5_000;
/// How long the uplink runs in each cycle of `trials` and `firstword`.
const CYCLE_STREAM_MS: u32 = 5_000;
/// The first sixteen cycles of `trials` start this far apart, which leaves 10 s of idle before each.
const TRIALS_SHORT_PERIOD_MS: u32 = 15_000;
/// How many cycles of `trials` follow the short idle.
const TRIALS_SHORT_CYCLES: u32 = 16;
/// The starts of the last four cycles of `trials`, each after 60 s of idle.
const TRIALS_LONG_STARTS_MS: [u32; 4] = [290_000, 355_000, 420_000, 485_000];
/// When `trials` tells the owner to stop counting.
const TRIALS_QUIET_MS: u32 = 70_000;
/// When `trials` ends.
const TRIALS_END_MS: u32 = 495_000;
/// The starts of the three cycles of `firstword`, each in the middle of the counting and after at least 8 s of idle.
const FIRSTWORD_STARTS_MS: [u32; 3] = [8_000, 23_000, 38_000];
/// When `firstword` tells the owner to stop counting.
const FIRSTWORD_QUIET_MS: u32 = 45_000;
/// When `firstword` ends.
const FIRSTWORD_END_MS: u32 = 48_000;

/// The cue `voice` shows before and after the speech, so the two taps bracket it.
const TAP_LEFT_TWICE: &str = "Tap the left bud twice";

/// `voice`: one uplink run with speech at three levels, two noises and a silence.
const VOICE: [Event; 11] = [
    at(0, Action::Start),
    at(3_000, Action::Cue(TAP_LEFT_TWICE)),
    at(
        8_000,
        Action::Cue("Count one to ten slowly, then say the fox sentence"),
    ),
    at(33_000, Action::Cue("Say the fox sentence quietly")),
    at(41_000, Action::Cue("Say the fox sentence loudly")),
    at(
        49_000,
        Action::Cue("Say sss for three seconds, then shh for three seconds"),
    ),
    at(59_000, Action::Cue("Jingle your keys for five seconds")),
    at(67_000, Action::Cue("Stay silent")),
    at(79_000, Action::Cue(TAP_LEFT_TWICE)),
    at(84_000, Action::Stop),
    at(89_000, Action::End),
];

/// `side`: one uplink run under each microphone side, then with each bud out of the ear.
const SIDE: [Event; 12] = [
    at(0, Action::Start),
    at(3_000, Action::SetSide(MicSide::Left)),
    at(
        3_000,
        Action::Cue("Say left left left. Tap the left bud twice, then the right bud twice"),
    ),
    at(18_000, Action::SetSide(MicSide::Right)),
    at(
        18_000,
        Action::Cue("Say right right right. Tap the left bud twice, then the right bud twice"),
    ),
    at(33_000, Action::RestoreSide),
    at(
        33_000,
        Action::Cue("Say automatic. Tap the left bud twice, then the right bud twice"),
    ),
    at(
        48_000,
        Action::Cue("Take the left bud out and hold it. Say one two three"),
    ),
    at(
        58_000,
        Action::Cue("Put it back. Take the right bud out. Say one two three"),
    ),
    at(68_000, Action::Cue("Put it back")),
    at(75_000, Action::Stop),
    at(80_000, Action::End),
];

/// `early`: the START went out before the handshake; a second run follows a normal one.
const EARLY: [Event; 4] = [
    at(5_000, Action::Stop),
    at(8_000, Action::Start),
    at(13_000, Action::Stop),
    at(18_000, Action::End),
];

/// The scripts, by the name `--script` takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScriptName {
    Knock,
    Listen,
    Soak,
    Voice,
    Side,
    Trials,
    Early,
    Firstword,
}

impl ScriptName {
    /// Every script, in the order the usage text lists them.
    pub(crate) const ALL: [Self; 8] = [
        Self::Knock,
        Self::Listen,
        Self::Soak,
        Self::Voice,
        Self::Side,
        Self::Trials,
        Self::Early,
        Self::Firstword,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Knock => "knock",
            Self::Listen => "listen",
            Self::Soak => "soak",
            Self::Voice => "voice",
            Self::Side => "side",
            Self::Trials => "trials",
            Self::Early => "early",
            Self::Firstword => "firstword",
        }
    }
}

impl fmt::Display for ScriptName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ScriptName {
    type Err = ScriptError;

    // Sample input: firstword, one of the eight names in lowercase.
    fn from_str(text: &str) -> Result<Self, ScriptError> {
        let mut names = Self::ALL.into_iter();
        names
            .find(|name| name.as_str() == text)
            .ok_or(ScriptError::UnknownName)
    }
}

/// Why no script was built; it never repeats the text it was given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ScriptError {
    #[error(
        "that is not a script; the scripts are knock, listen, soak, voice, side, trials, early and firstword"
    )]
    UnknownName,
    #[error("script {name} runs for a set time: pass --minutes {MIN_MINUTES} to {MAX_MINUTES}")]
    MinutesRequired { name: ScriptName },
    #[error("script {name} has a fixed length: --minutes is only for listen and soak")]
    MinutesRefused { name: ScriptName },
    #[error("--minutes {minutes} is outside {MIN_MINUTES} to {MAX_MINUTES}")]
    MinutesOutOfRange { minutes: u32 },
}

/// What a run does once the channel is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Opening {
    /// The handshake, then the gate.
    Handshake,
    /// Nothing is sent; events count from the connect.
    Knock,
    /// START goes out before the handshake.
    EarlyStart,
}

/// One thing a script does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Start,
    Stop,
    /// A line shown to the owner.
    Cue(&'static str),
    SetSide(MicSide),
    /// Puts the microphone side back to the value the pods reported.
    RestoreSide,
    End,
}

/// An action and its offset from the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Event {
    pub(crate) at_ms: u32,
    pub(crate) action: Action,
}

/// A whole run: how it opens and what follows, in the order it happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Script {
    pub(crate) opening: Opening,
    /// The run needs the microphone side the pods report before it may start.
    pub(crate) needs_initial_side: bool,
    pub(crate) events: Vec<Event>,
}

const fn at(at_ms: u32, action: Action) -> Event {
    Event { at_ms, action }
}

/// Builds the script `name`; `minutes` is how long `listen` and `soak` run, and no other script takes it.
// Sample input: (soak, Some(2)) is two minutes of uplink; (voice, None) is the fixed voice script.
pub(crate) fn build(name: ScriptName, minutes: Option<u32>) -> Result<Script, ScriptError> {
    let mut events = match (name, minutes) {
        (ScriptName::Listen, Some(minutes)) => vec![at(run_ms(minutes)?, Action::End)],
        (ScriptName::Soak, Some(minutes)) => soak(run_ms(minutes)?),
        (ScriptName::Listen | ScriptName::Soak, None) => {
            return Err(ScriptError::MinutesRequired { name });
        }
        (ScriptName::Knock, None) => vec![at(KNOCK_HOLD_MS, Action::End)],
        (ScriptName::Voice, None) => VOICE.to_vec(),
        (ScriptName::Side, None) => SIDE.to_vec(),
        (ScriptName::Trials, None) => trials(),
        (ScriptName::Early, None) => EARLY.to_vec(),
        (ScriptName::Firstword, None) => firstword(),
        (
            ScriptName::Knock
            | ScriptName::Voice
            | ScriptName::Side
            | ScriptName::Trials
            | ScriptName::Early
            | ScriptName::Firstword,
            Some(_),
        ) => return Err(ScriptError::MinutesRefused { name }),
    };
    // The sort is stable, so events at one offset keep the order they are written in.
    events.sort_by_key(|event| event.at_ms);
    let opening = match name {
        ScriptName::Knock => Opening::Knock,
        ScriptName::Early => Opening::EarlyStart,
        ScriptName::Listen
        | ScriptName::Soak
        | ScriptName::Voice
        | ScriptName::Side
        | ScriptName::Trials
        | ScriptName::Firstword => Opening::Handshake,
    };
    Ok(Script {
        opening,
        needs_initial_side: name == ScriptName::Side,
        events,
    })
}

// Sample input: 2, read as 120000 milliseconds; 0 and 91 are refused.
fn run_ms(minutes: u32) -> Result<u32, ScriptError> {
    if (MIN_MINUTES..=MAX_MINUTES).contains(&minutes) {
        Ok(minutes.saturating_mul(MS_PER_MINUTE))
    } else {
        Err(ScriptError::MinutesOutOfRange { minutes })
    }
}

/// `soak`: one uplink run of `run_ms` with nothing asked of the owner.
fn soak(run_ms: u32) -> Vec<Event> {
    vec![
        at(0, Action::Start),
        at(0, Action::Cue("Stay as you are. Nothing to do.")),
        at(run_ms, Action::Stop),
        at(run_ms.saturating_add(SOAK_TAIL_MS), Action::End),
    ]
}

/// One uplink run of `CYCLE_STREAM_MS` for each start.
fn cycles(starts_ms: impl IntoIterator<Item = u32>) -> impl Iterator<Item = Event> {
    starts_ms.into_iter().flat_map(|start_ms| {
        let stop_ms = start_ms.saturating_add(CYCLE_STREAM_MS);
        [at(start_ms, Action::Start), at(stop_ms, Action::Stop)]
    })
}

/// `trials`: twenty uplink runs while the owner counts and then stays quiet, sixteen after 10 s of idle and four after 60 s.
fn trials() -> Vec<Event> {
    let short_starts = (0..TRIALS_SHORT_CYCLES).map(|k| k.saturating_mul(TRIALS_SHORT_PERIOD_MS));
    let mut events = vec![at(0, Action::Cue("Count aloud until told to stop"))];
    events.extend(cycles(short_starts.chain(TRIALS_LONG_STARTS_MS)));
    let quiet = Action::Cue("Stop counting. Stay quiet.");
    events.extend([at(TRIALS_QUIET_MS, quiet), at(TRIALS_END_MS, Action::End)]);
    events
}

/// `firstword`: three uplink runs that begin while the owner is already counting, with no cue at a start.
fn firstword() -> Vec<Event> {
    let counting = Action::Cue("Count aloud from one and do not stop until told");
    let mut events = vec![at(0, counting)];
    events.extend(cycles(FIRSTWORD_STARTS_MS));
    let quiet = Action::Cue("Stop counting");
    events.extend([
        at(FIRSTWORD_QUIET_MS, quiet),
        at(FIRSTWORD_END_MS, Action::End),
    ]);
    events
}

/// The rescue: after the handshake, a STOP for an uplink that a killed capture left running.
pub(crate) fn rescue() -> Script {
    Script {
        opening: Opening::Handshake,
        needs_initial_side: false,
        events: vec![at(0, Action::Stop), at(0, Action::End)],
    }
}

#[cfg(test)]
mod tests;
