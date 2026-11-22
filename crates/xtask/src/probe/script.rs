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
/// How long `knock` and `att-knock` hold the channel open after the connect.
const KNOCK_HOLD_MS: u32 = 3_000;
/// How long `soak` keeps logging after its STOP, on either route.
const SOAK_TAIL_MS: u32 = 5_000;
/// How long the active session marker leads the start write: a Mac was seen to send it 446 to 508 ms before, in three sessions with one pair of pods.
const MARKER_LEAD_MS: u32 = 480;
/// How long after the stop write the idle session marker follows in every script that starts the dictation stream.
const IDLE_LAG_MS: u32 = 500;
/// When `voice` stops its stream, on either route.
const VOICE_STOP_MS: u32 = 84_000;
/// When `voice` ends, on either route.
const VOICE_END_MS: u32 = 89_000;
/// When `both` starts the uplink, with the dictation stream already running.
const BOTH_UPLINK_START_MS: u32 = 5_000;
/// When `both` stops the uplink.
const BOTH_UPLINK_STOP_MS: u32 = 60_000;
/// When `both` stops the dictation stream, after the uplink.
const BOTH_DICTATION_STOP_MS: u32 = 65_000;
/// When `both` ends.
const BOTH_END_MS: u32 = 70_000;
/// How long the stream runs in each cycle of `trials` and `firstword`, on either route.
const CYCLE_STREAM_MS: u32 = 5_000;
/// The first sixteen cycles of `trials` start this far apart, which leaves 10 s of idle between them.
const TRIALS_SHORT_PERIOD_MS: u32 = 15_000;
/// How many cycles of `trials` start that close together; the first of them starts at the gate.
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

/// The three cues of `voice` that ask for speech, at three efforts.
const SPEAK: [&str; 3] = [
    "Count one to ten slowly, then say the fox sentence",
    "Say the fox sentence quietly",
    "Say the fox sentence loudly",
];
const SAY_SSS: &str = "Say sss for three seconds, then shh for three seconds";
const JINGLE_KEYS: &str = "Jingle your keys for five seconds";
const STAY_SILENT: &str = "Stay silent";

/// What a cue of `voice` asks the owner for, which is what is then measured in its window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Asked {
    Taps,
    Speech,
    Sibilant,
    Jingle,
    Silence,
}

impl Asked {
    // Sample input: `Stay silent`, the text of a cue as the marks file holds it; a cue no measure reads gives `None`.
    pub(crate) fn by(cue: &str) -> Option<Self> {
        match cue {
            TAP_LEFT_TWICE => Some(Self::Taps),
            SAY_SSS => Some(Self::Sibilant),
            JINGLE_KEYS => Some(Self::Jingle),
            STAY_SILENT => Some(Self::Silence),
            _ => SPEAK.contains(&cue).then_some(Self::Speech),
        }
    }
}

/// `voice`: one uplink run with speech at three levels, two noises and a silence.
const VOICE: [Event; 11] = [
    at(0, Action::Start),
    at(3_000, Action::Cue(TAP_LEFT_TWICE)),
    at(8_000, Action::Cue(SPEAK[0])),
    at(33_000, Action::Cue(SPEAK[1])),
    at(41_000, Action::Cue(SPEAK[2])),
    at(49_000, Action::Cue(SAY_SSS)),
    at(59_000, Action::Cue(JINGLE_KEYS)),
    at(67_000, Action::Cue(STAY_SILENT)),
    at(79_000, Action::Cue(TAP_LEFT_TWICE)),
    at(VOICE_STOP_MS, Action::Stop),
    at(VOICE_END_MS, Action::End),
];

/// `side`: one uplink run under each microphone side, then with each bud out of the ear.
const SIDE: [Event; 13] = [
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
    at(33_000, Action::SetSide(MicSide::Automatic)),
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
    at(77_000, Action::RestoreSide),
    at(80_000, Action::End),
];

/// `early`: the START went out before the handshake; a second run follows a normal one.
const EARLY: [Event; 4] = [
    at(5_000, Action::Stop),
    at(8_000, Action::Start),
    at(13_000, Action::Stop),
    at(18_000, Action::End),
];

/// Which microphone stream a run asks the pods for, by the name `--route` takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    /// The stream on the control channel.
    Uplink,
    /// The stream in notifications on the attribute channel.
    Dictation,
    /// Both streams in one run.
    Both,
}

impl Route {
    /// Every route, in the order the usage text lists them.
    pub(crate) const ALL: [Self; 3] = [Self::Uplink, Self::Dictation, Self::Both];

    fn as_str(self) -> &'static str {
        match self {
            Self::Uplink => "uplink",
            Self::Dictation => "dictation",
            Self::Both => "both",
        }
    }

    /// The scripts of this route as a refusal lists them; a test holds the list equal to what `build` takes.
    fn scripts(self) -> &'static str {
        match self {
            Self::Uplink => "knock, listen, soak, voice, side, trials, early and firstword",
            Self::Dictation => "soak, voice, firstword, att-knock and att-map",
            Self::Both => "both",
        }
    }
}

impl fmt::Display for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Route {
    type Err = ScriptError;

    // Sample input: `dictation`, one of the route names in lowercase.
    fn from_str(text: &str) -> Result<Self, ScriptError> {
        let mut routes = Self::ALL.into_iter();
        routes
            .find(|route| route.as_str() == text)
            .ok_or(ScriptError::UnknownRoute)
    }
}

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
    AttKnock,
    AttMap,
    Both,
}

impl ScriptName {
    /// Every script, in the order the usage text lists them.
    pub(crate) const ALL: [Self; 11] = [
        Self::Knock,
        Self::Listen,
        Self::Soak,
        Self::Voice,
        Self::Side,
        Self::Trials,
        Self::Early,
        Self::Firstword,
        Self::AttKnock,
        Self::AttMap,
        Self::Both,
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
            Self::AttKnock => "att-knock",
            Self::AttMap => "att-map",
            Self::Both => "both",
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

    // Sample input: `firstword`, one of the script names in lowercase.
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
        "that is not a script; the scripts are knock, listen, soak, voice, side, trials, early, firstword, att-knock, att-map and both"
    )]
    UnknownName,
    #[error("that is not a route; the routes are uplink, dictation and both")]
    UnknownRoute,
    #[error(
        "script {name} is not a script of the {route} route, whose scripts are {scripts}",
        scripts = .route.scripts()
    )]
    NotOnRoute { name: ScriptName, route: Route },
    #[error("script {name} runs for a set time: pass --minutes {MIN_MINUTES} to {MAX_MINUTES}")]
    MinutesRequired { name: ScriptName },
    #[error("script {name} has a fixed length: --minutes is only for listen and soak")]
    MinutesRefused { name: ScriptName },
    #[error("--minutes {minutes} is outside {MIN_MINUTES} to {MAX_MINUTES}")]
    MinutesOutOfRange { minutes: u32 },
    #[error("--no-marker is refused on the uplink route, which sends no session marker")]
    NoMarkerOnUplink,
    #[error("--unchecked is refused on the uplink route, which checks no handle")]
    UncheckedOnUplink,
    #[error(
        "stop takes one route at a time: run it with --route uplink, then with --route dictation"
    )]
    RescueOfBoth,
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
    /// Only the attribute channel is open and nothing is sent; events count from the connect.
    AttKnock,
    /// Only the attribute channel is open; the table is walked and its declarations are read, then the gate opens.
    AttMap,
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
    /// The session marker a Mac sends on the control channel before the start write.
    MarkActive,
    /// The session marker a Mac sends on the control channel after the stop write.
    MarkIdle,
    /// The write that starts the dictation stream.
    DictationStart,
    /// The write that stops the dictation stream.
    DictationStop,
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
    /// The owner allowed the start write after a walk the pods did not answer.
    pub(crate) unchecked: bool,
    pub(crate) events: Vec<Event>,
}

impl Script {
    /// Whether the run reads the pods' attribute table before its gate: to map it, or to check it before a start write.
    pub(crate) fn walks(&self) -> bool {
        let mut actions = self.events.iter().map(|event| event.action);
        self.opening == Opening::AttMap || actions.any(|action| action == Action::DictationStart)
    }

    /// Lets the start write follow a walk that the pods did not answer, which `--unchecked` asks for; `route` is the one the script was built for.
    pub(crate) fn allow_unchecked(mut self, route: Route) -> Result<Self, ScriptError> {
        match route {
            Route::Uplink => Err(ScriptError::UncheckedOnUplink),
            Route::Dictation | Route::Both => {
                self.unchecked = true;
                Ok(self)
            }
        }
    }
}

const fn at(at_ms: u32, action: Action) -> Event {
    Event { at_ms, action }
}

/// Builds the script `name` of `route`, as `(soak, uplink, Some(2), true)` builds two minutes of uplink; `minutes` is how long `listen` and `soak` run, and `marker` false drops the two session markers and nothing else.
pub(crate) fn build(
    name: ScriptName,
    route: Route,
    minutes: Option<u32>,
    marker: bool,
) -> Result<Script, ScriptError> {
    let (opening, mut events) = match route {
        Route::Uplink if !marker => return Err(ScriptError::NoMarkerOnUplink),
        Route::Uplink => uplink(name, minutes)?,
        Route::Dictation => dictation(name, minutes)?,
        Route::Both => both(name, minutes)?,
    };
    if !marker {
        events.retain(|event| !matches!(event.action, Action::MarkActive | Action::MarkIdle));
    }
    // The sort is stable, so events at one offset keep the order they are written in.
    events.sort_by_key(|event| event.at_ms);
    Ok(Script {
        opening,
        needs_initial_side: route == Route::Uplink && name == ScriptName::Side,
        unchecked: false,
        events,
    })
}

/// How a script of the uplink route opens and what it does.
fn uplink(name: ScriptName, minutes: Option<u32>) -> Result<(Opening, Vec<Event>), ScriptError> {
    let script = match (name, minutes) {
        (ScriptName::Listen, Some(minutes)) => {
            let end = at(run_ms(minutes)?, Action::End);
            (Opening::Handshake, vec![end])
        }
        (ScriptName::Soak, Some(minutes)) => (Opening::Handshake, soak(run_ms(minutes)?)),
        (ScriptName::Listen | ScriptName::Soak, None) => {
            return Err(ScriptError::MinutesRequired { name });
        }
        (ScriptName::Knock, None) => (Opening::Knock, vec![at(KNOCK_HOLD_MS, Action::End)]),
        (ScriptName::Voice, None) => (Opening::Handshake, VOICE.to_vec()),
        (ScriptName::Side, None) => (Opening::Handshake, SIDE.to_vec()),
        (ScriptName::Trials, None) => (Opening::Handshake, trials()),
        (ScriptName::Early, None) => (Opening::EarlyStart, EARLY.to_vec()),
        (ScriptName::Firstword, None) => (Opening::Handshake, firstword()),
        (
            ScriptName::Knock
            | ScriptName::Voice
            | ScriptName::Side
            | ScriptName::Trials
            | ScriptName::Early
            | ScriptName::Firstword,
            Some(_),
        ) => return Err(ScriptError::MinutesRefused { name }),
        (ScriptName::AttKnock | ScriptName::AttMap | ScriptName::Both, _) => {
            let route = Route::Uplink;
            return Err(ScriptError::NotOnRoute { name, route });
        }
    };
    Ok(script)
}

/// How a script of the dictation route opens and what it does.
fn dictation(name: ScriptName, minutes: Option<u32>) -> Result<(Opening, Vec<Event>), ScriptError> {
    let script = match (name, minutes) {
        (ScriptName::AttKnock, None) => (Opening::AttKnock, vec![at(KNOCK_HOLD_MS, Action::End)]),
        // The walk and the one read end before the gate, so nothing is left for the script to wait on.
        (ScriptName::AttMap, None) => (Opening::AttMap, vec![at(0, Action::End)]),
        (ScriptName::Soak, Some(minutes)) => (Opening::Handshake, dictation_soak(run_ms(minutes)?)),
        (ScriptName::Soak, None) => return Err(ScriptError::MinutesRequired { name }),
        (ScriptName::Voice, None) => (Opening::Handshake, dictation_voice()),
        (ScriptName::Firstword, None) => (Opening::Handshake, dictation_firstword()),
        (
            ScriptName::AttKnock | ScriptName::AttMap | ScriptName::Voice | ScriptName::Firstword,
            Some(_),
        ) => return Err(ScriptError::MinutesRefused { name }),
        (
            ScriptName::Knock
            | ScriptName::Listen
            | ScriptName::Side
            | ScriptName::Trials
            | ScriptName::Early
            | ScriptName::Both,
            _,
        ) => {
            let route = Route::Dictation;
            return Err(ScriptError::NotOnRoute { name, route });
        }
    };
    Ok(script)
}

/// The one script of the route that runs both streams.
fn both(name: ScriptName, minutes: Option<u32>) -> Result<(Opening, Vec<Event>), ScriptError> {
    match (name, minutes) {
        (ScriptName::Both, None) => Ok((Opening::Handshake, both_streams())),
        (ScriptName::Both, Some(_)) => Err(ScriptError::MinutesRefused { name }),
        (
            ScriptName::Knock
            | ScriptName::Listen
            | ScriptName::Soak
            | ScriptName::Voice
            | ScriptName::Side
            | ScriptName::Trials
            | ScriptName::Early
            | ScriptName::Firstword
            | ScriptName::AttKnock
            | ScriptName::AttMap,
            _,
        ) => {
            let route = Route::Both;
            Err(ScriptError::NotOnRoute { name, route })
        }
    }
}

// Sample input: `2`, read as 120000 milliseconds; 0 and 91 are refused.
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

/// `trials`: twenty uplink runs while the owner counts and then stays quiet: one at the gate, fifteen after 10 s of idle and four after 60 s.
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

/// One dictation stream that ends at `stop_ms`: the marker, the start write `MARKER_LEAD_MS` later, the stop write, and the idle marker `IDLE_LAG_MS` after it.
fn dictation_run(stop_ms: u32) -> [Event; 4] {
    [
        at(0, Action::MarkActive),
        at(MARKER_LEAD_MS, Action::DictationStart),
        at(stop_ms, Action::DictationStop),
        at(stop_ms.saturating_add(IDLE_LAG_MS), Action::MarkIdle),
    ]
}

/// The dictation `soak`: one stream until `run_ms` with nothing asked of the owner.
fn dictation_soak(run_ms: u32) -> Vec<Event> {
    let mut events = dictation_run(run_ms).to_vec();
    let still = Action::Cue("Stay as you are. Nothing to do.");
    let end = at(run_ms.saturating_add(SOAK_TAIL_MS), Action::End);
    events.extend([at(MARKER_LEAD_MS, still), end]);
    events
}

/// The dictation `voice`: one stream under the cues of the uplink `voice`, at the same offsets.
fn dictation_voice() -> Vec<Event> {
    let mut events = dictation_run(VOICE_STOP_MS).to_vec();
    let cues = VOICE.into_iter();
    events.extend(cues.filter(|event| matches!(event.action, Action::Cue(_))));
    events.push(at(VOICE_END_MS, Action::End));
    events
}

/// The dictation `firstword`: three streams that begin while the owner is already counting, each start write led by its marker.
fn dictation_firstword() -> Vec<Event> {
    let counting = Action::Cue("Count aloud from one and do not stop until told");
    let mut events = vec![at(0, counting)];
    for start_ms in FIRSTWORD_STARTS_MS {
        let stop_ms = start_ms.saturating_add(CYCLE_STREAM_MS);
        events.extend([
            at(start_ms.saturating_sub(MARKER_LEAD_MS), Action::MarkActive),
            at(start_ms, Action::DictationStart),
            at(stop_ms, Action::DictationStop),
            at(stop_ms.saturating_add(IDLE_LAG_MS), Action::MarkIdle),
        ]);
    }
    let quiet = Action::Cue("Stop counting");
    events.extend([
        at(FIRSTWORD_QUIET_MS, quiet),
        at(FIRSTWORD_END_MS, Action::End),
    ]);
    events
}

/// `both`: the uplink starts and stops while the dictation stream runs, and the owner counts aloud from the uplink's start.
fn both_streams() -> Vec<Event> {
    let mut events = dictation_run(BOTH_DICTATION_STOP_MS).to_vec();
    events.extend([
        at(BOTH_UPLINK_START_MS, Action::Start),
        at(BOTH_UPLINK_START_MS, Action::Cue("Count aloud to twenty")),
        at(BOTH_UPLINK_STOP_MS, Action::Stop),
        at(BOTH_END_MS, Action::End),
    ]);
    events
}

/// The rescue of one route: after the handshake, the stop that a killed capture did not send, and for dictation the idle marker after it.
pub(crate) fn rescue(route: Route) -> Result<Script, ScriptError> {
    let events = match route {
        Route::Uplink => vec![at(0, Action::Stop), at(0, Action::End)],
        Route::Dictation => vec![
            at(0, Action::DictationStop),
            at(0, Action::MarkIdle),
            at(0, Action::End),
        ],
        Route::Both => return Err(ScriptError::RescueOfBoth),
    };
    Ok(Script {
        opening: Opening::Handshake,
        needs_initial_side: false,
        unchecked: false,
        events,
    })
}

#[cfg(test)]
mod tests;
