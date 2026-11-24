//! The pods' side of the dictation service: the table an arm reads, what a START and a STOP are answered with and when each frame is due, with no socket or thread.

mod table;
#[cfg(test)]
mod tests;

use std::collections::VecDeque;
use std::mem;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pods_samples::doap::{self as sample, Handles, REASON_UNASKED, host};
use pods_samples::{DOAP_MODE, opus, setting};

use crate::serve::Shared;

/// The audio one frame holds, which is the time between two frames.
pub const FRAME: Duration = Duration::from_millis(20);
/// The frames the microphone keeps, 1.6 s: what a start can still send of the time before it.
pub const KEPT_FRAMES: usize = 80;
/// The frames a start sends at once and how many go in a notification, until a test sets others; as a Mac was seen to get them.
pub const BURST_FRAMES: u32 = 63;
pub const BURST_PER_NOTIFICATION: u32 = 5;
/// How far `Table::Shifted` stands above the observed table.
pub const SHIFT: u16 = 0x20;
/// What a garbled frame opens with: the table of contents byte of a 10 ms frame, which the stream never holds.
pub const GARBLED_TOC: u8 = 0xb0;

/// The table the pods serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Table {
    /// One firmware's.
    Observed,
    /// The same with every handle `SHIFT` higher, as another firmware may hold it.
    Shifted,
    /// No characteristic at all, as a model without the service.
    Absent,
}

impl Table {
    fn handles(self) -> Option<Handles> {
        match self {
            Self::Observed => Some(Handles::OBSERVED),
            Self::Shifted => Some(Handles::OBSERVED.shifted(SHIFT)),
            Self::Absent => None,
        }
    }
}

/// The test's hand on the dictation service of one pair of pods; every clone is the same service.
#[derive(Debug, Clone)]
pub struct Doap {
    shared: Arc<Shared>,
}

impl Doap {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    /// The table every request from now on is answered from.
    pub fn table(&self, table: Table) {
        self.shared.doap(|doap| doap.table = table);
    }

    /// The next `starts` starts are refused with reason 6 even with the mode set.
    pub fn refuse_starts(&self, starts: u32) {
        self.shared.doap(|doap| doap.refusals = starts);
    }

    /// While held a START is heard and not answered until `release_start`.
    pub fn hold_start(&self, hold: bool) {
        self.shared.doap(|doap| doap.hold_start = hold);
    }

    /// Answers the held START as one that just came: the score, the burst, then audio.
    pub fn release_start(&self) {
        self.shared.doap(|doap| doap.release_start(Instant::now()));
    }

    /// How many frames a start sends at once, and how many go in a notification.
    pub fn burst(&self, frames: u32, per_notification: u32) {
        self.shared
            .doap(|doap| doap.burst = (frames, per_notification));
    }

    /// The next `frames` frames the microphone hears hold the second tone, streaming or not.
    pub fn mark(&self, frames: u32) {
        self.shared.doap(|doap| {
            doap.hear(Instant::now());
            doap.marked = frames;
        });
    }

    /// The counter of the next frame the microphone hears.
    pub fn start_counter(&self, counter: u16) {
        self.shared.doap(|doap| {
            doap.hear(Instant::now());
            doap.next_counter = counter;
        });
    }

    /// The burst of the next start sends the frame `counter_offset` after its first twice.
    pub fn repeat_in_burst(&self, counter_offset: u16) {
        self.shared.doap(|doap| doap.repeat = Some(counter_offset));
    }

    /// The next `frames` frames of a stream are heard and never sent, so the counter passes over them.
    pub fn lose(&self, frames: u32) {
        self.shared.doap(|doap| doap.lost = frames);
    }

    /// The next `frames` frames of a stream open with `GARBLED_TOC`.
    pub fn garble(&self, frames: u32) {
        self.shared.doap(|doap| doap.garbled = frames);
    }

    /// Holds the next frame until `gap` after the audio notification before it; the frames heard meanwhile go out with it, each in its own notification. With no audio notification before it in the stream it holds nothing and is spent.
    pub fn stall(&self, gap: Duration) {
        self.shared.doap(|doap| doap.stall = Some(gap));
    }

    /// The pods end the stream with no STOP heard.
    pub fn end_unasked(&self) {
        self.shared.doap(Machine::end_unasked);
    }

    /// Sends the last `frames` frames the microphone heard with no START heard, each in its own notification.
    pub fn audio_unasked(&self, frames: u32) {
        self.shared
            .doap(|doap| doap.audio_unasked(frames, Instant::now()));
    }

    /// The pods ask the host something under `opcode` on every open attribute channel.
    pub fn send_request(&self, opcode: u8) {
        self.shared
            .doap(|doap| doap.owe_everywhere(&sample::request(opcode)));
    }

    /// The pods send an indication on every open attribute channel.
    pub fn indicate(&self) {
        self.shared
            .doap(|doap| doap.owe_everywhere(&sample::UNASKED_INDICATION));
    }

    /// The STARTs heard, answered or not.
    #[must_use]
    pub fn starts(&self) -> u32 {
        self.shared.doap(|doap| doap.starts)
    }

    /// The reason of every STOP heard, in order.
    #[must_use]
    pub fn stops(&self) -> Vec<u8> {
        self.shared.doap(|doap| doap.stops.clone())
    }

    /// Whether the mode was set on the control channel that is open now.
    #[must_use]
    pub fn mode_seen(&self) -> bool {
        self.shared.doap(|doap| doap.mic.is_some())
    }

    /// From a START heard with the mode set, or its release, until the end of its stream; the score may still be on its way.
    #[must_use]
    pub fn streaming(&self) -> bool {
        self.shared.doap(|doap| doap.run.is_some())
    }

    /// How many error responses and how many confirmations the host sent: its answers to the pods' own requests and indications.
    #[must_use]
    pub fn answered(&self) -> (u32, u32) {
        self.shared.doap(|doap| (doap.refused, doap.confirmed))
    }

    /// The longest time between two audio notifications sent since the last stream began; none until it sent two.
    #[must_use]
    pub fn largest_gap(&self) -> Option<Duration> {
        self.shared.doap(|doap| doap.largest_gap)
    }
}

#[derive(Debug)]
pub(crate) struct Machine {
    table: Table,
    refusals: u32,
    hold_start: bool,
    /// The channel of a START heard and not answered.
    held: Option<usize>,
    burst: (u32, u32),
    repeat: Option<u16>,
    marked: u32,
    lost: u32,
    garbled: u32,
    stall: Option<Duration>,
    next_counter: u16,
    mic: Option<Mic>,
    run: Option<Run>,
    /// Every open attribute channel with the descriptors the host wrote on it.
    lines: Vec<(usize, Vec<u16>)>,
    /// What a test made the pods send on a channel, oldest first.
    owed: VecDeque<(usize, Vec<u8>)>,
    starts: u32,
    stops: Vec<u8>,
    refused: u32,
    confirmed: u32,
    largest_gap: Option<Duration>,
    tone: Vec<&'static [u8]>,
    mark: Vec<&'static [u8]>,
}

/// The microphone, which hears a frame every `FRAME` from the moment the mode is set on `control`.
#[derive(Debug)]
struct Mic {
    control: usize,
    since: Instant,
    /// The frames heard so far, which is the place of the next one.
    heard: u32,
    kept: VecDeque<Heard>,
}

#[derive(Debug, Clone, Copy)]
struct Heard {
    /// Counts from the moment the mode was set and picks the frame of the fixture.
    place: u32,
    counter: u16,
    marked: bool,
}

/// The stream from the answer to a START until its end, on the channel the START came on.
#[derive(Debug)]
struct Run {
    line: usize,
    /// Whether the score and the burst have gone out.
    announced: bool,
    /// The counter of the next audio notification.
    packet: u8,
    /// The place of the next frame to send.
    next: u32,
    /// When the last audio notification was sent.
    last: Option<Instant>,
}

impl Machine {
    pub(crate) fn new() -> Self {
        Self {
            table: Table::Observed,
            refusals: 0,
            hold_start: false,
            held: None,
            burst: (BURST_FRAMES, BURST_PER_NOTIFICATION),
            repeat: None,
            marked: 0,
            lost: 0,
            garbled: 0,
            stall: None,
            next_counter: 0,
            mic: None,
            run: None,
            lines: Vec::new(),
            owed: VecDeque::new(),
            starts: 0,
            stops: Vec::new(),
            refused: 0,
            confirmed: 0,
            largest_gap: None,
            tone: opus::tone().collect(),
            mark: opus::mark().collect(),
        }
    }

    /// A frame the host sent on control channel `line` at `now`; only the mode is this service's.
    pub(crate) fn on_control(&mut self, line: usize, sdu: &[u8], now: Instant) {
        // corner: assumed until measured on hardware, the mode set again changes nothing
        if self.mic.is_none() && sdu == setting(DOAP_MODE, 1) {
            self.mic = Some(Mic {
                control: line,
                since: now,
                heard: 0,
                kept: VecDeque::new(),
            });
        }
    }

    /// An attribute channel opened as `line`.
    pub(crate) fn opened(&mut self, line: usize) {
        self.lines.push((line, Vec::new()));
    }

    /// Channel `line` ended: with the control channel the mode goes, and the microphone and the stream with it.
    pub(crate) fn closed(&mut self, line: usize) {
        if self.mic.as_ref().is_some_and(|mic| mic.control == line) {
            (self.mic, self.run, self.held) = (None, None, None);
        }
        if self.run.as_ref().is_some_and(|run| run.line == line) {
            self.run = None;
        }
        if self.held == Some(line) {
            self.held = None;
        }
        self.lines.retain(|(open, _)| *open != line);
        self.owed.retain(|(to, _)| *to != line);
    }

    /// The answers to one packet the host sent on attribute channel `line` at `now`.
    pub(crate) fn on_att(&mut self, line: usize, pdu: &[u8], now: Instant) -> Vec<Vec<u8>> {
        if pdu.first() == Some(&sample::ERROR_RESPONSE) {
            self.refused = self.refused.saturating_add(1);
            return Vec::new();
        }
        if pdu == host::CONFIRMED {
            self.confirmed = self.confirmed.saturating_add(1);
            return Vec::new();
        }
        let handles = self.table.handles();
        let Some(served) = handles else {
            return table::answer(None, pdu).into_iter().collect();
        };
        let [_, stop_descriptor, _, event_descriptor] = served.cccds;
        if pdu == host::start(&served) {
            let refusal = sample::stopped(&served, REASON_UNASKED).to_vec();
            let refused = self.on_start(line, now).then_some(refusal);
            return self
                .notify(line, stop_descriptor, refused)
                .into_iter()
                .collect();
        }
        if let Some(reason) = pdu.last().filter(|last| pdu == host::stop(&served, **last)) {
            self.stops.push(*reason);
            // corner: assumed until measured on hardware, a STOP with no stream running is answered with nothing
            let ended = self.run.take_if(|run| run.line == line);
            let end = ended.map(|_| sample::end(&served));
            return self
                .notify(line, event_descriptor, end)
                .into_iter()
                .collect();
        }
        let mut written = served.cccds.into_iter();
        if let Some(descriptor) = written.find(|descriptor| pdu == host::subscribe(*descriptor)) {
            let open = self.lines.iter_mut().find(|(open, _)| *open == line);
            if let Some((_, descriptors)) = open
                && !descriptors.contains(&descriptor)
            {
                descriptors.push(descriptor);
            }
            return vec![sample::WRITTEN.to_vec()];
        }
        table::answer(Some(&served), pdu).into_iter().collect()
    }

    /// What is due on attribute channel `line` at `now`, oldest first: what a test queued, the score and the burst of a stream just started, and every frame heard since.
    pub(crate) fn due(&mut self, line: usize, now: Instant) -> Vec<Vec<u8>> {
        let mut due = Vec::new();
        self.owed.retain(|(to, pdu)| {
            let ours = *to == line;
            if ours {
                due.push(pdu.clone());
            }
            !ours
        });
        let Some(handles) = self.table.handles() else {
            return due;
        };
        // Heard on any line, since hearing follows the clock alone, and before the stream is taken out, since `hear` keeps what the stream has still to send.
        self.hear(now);
        let Some(mut run) = self.run.take_if(|run| run.line == line) else {
            return due;
        };
        let [.., event_descriptor] = handles.cccds;
        if !mem::replace(&mut run.announced, true) {
            let score = Some(sample::score(&handles));
            due.extend(self.notify(line, event_descriptor, score));
            let (_, per_notification) = self.burst;
            let per_notification = usize::try_from(per_notification.max(1)).unwrap_or(usize::MAX);
            for frames in self.opening(run.next).chunks(per_notification) {
                due.extend(self.audio(&handles, &mut run, frames, now));
            }
        }
        if self.held_back(&run, now).is_zero() {
            let fresh = self.kept_from(run.next);
            for heard in fresh {
                self.stall = None;
                run.next = heard.place.saturating_add(1);
                due.extend(self.audio(&handles, &mut run, &[heard], now));
            }
        }
        self.run = Some(run);
        due
    }

    /// How long attribute channel `line` may wait at `now` before something is due on it; none while nothing is coming.
    pub(crate) fn wait(&self, line: usize, now: Instant) -> Option<Duration> {
        if self.owed.iter().any(|(to, _)| *to == line) {
            return Some(Duration::ZERO);
        }
        // A table that holds nothing sends nothing of a stream, so its channel sleeps.
        let served = self.table.handles().is_some();
        let run = self.run.as_ref().filter(|run| served && run.line == line)?;
        let mic = self.mic.as_ref()?;
        if !run.announced {
            return Some(Duration::ZERO);
        }
        let heard_after = FRAME.saturating_mul(run.next.saturating_add(1));
        let on_schedule = heard_after.saturating_sub(now.saturating_duration_since(mic.since));
        Some(on_schedule.max(self.held_back(run, now)))
    }

    /// Whether the START just heard on `line` is refused; one that is not starts the stream, or waits while starts are held.
    fn on_start(&mut self, line: usize, now: Instant) -> bool {
        self.starts = self.starts.saturating_add(1);
        if self.mic.is_none() {
            return true;
        }
        if self.refusals > 0 {
            self.refusals = self.refusals.saturating_sub(1);
            return true;
        }
        // corner: assumed until measured on hardware, a START while the stream runs or another START waits changes nothing
        if self.run.is_some() || self.held.is_some() {
        } else if self.hold_start {
            self.held = Some(line);
        } else {
            self.start(line, now);
        }
        false
    }

    fn start(&mut self, line: usize, now: Instant) {
        self.hear(now);
        self.largest_gap = None;
        self.run = Some(Run {
            line,
            announced: false,
            packet: 0,
            next: self.mic.as_ref().map_or(0, |mic| mic.heard),
            last: None,
        });
    }

    fn release_start(&mut self, now: Instant) {
        if let Some(line) = self.held.take() {
            self.start(line, now);
        }
    }

    fn end_unasked(&mut self) {
        let Some(handles) = self.table.handles() else {
            return;
        };
        let [.., event_descriptor] = handles.cccds;
        if let Some(run) = self.run.take() {
            let end = self.notify(run.line, event_descriptor, Some(sample::end(&handles)));
            self.owed.extend(end.into_iter().map(|end| (run.line, end)));
        }
    }

    fn audio_unasked(&mut self, frames: u32, now: Instant) {
        let Some(handles) = self.table.handles() else {
            return;
        };
        self.hear(now);
        let [_, _, audio_descriptor, _] = handles.cccds;
        let kept = self.kept_from(0);
        let frames = usize::try_from(frames).unwrap_or(usize::MAX);
        let newest = kept.get(kept.len().saturating_sub(frames)..);
        let lines: Vec<usize> = self.lines.iter().map(|(line, _)| *line).collect();
        for line in lines {
            for (packet, heard) in (0_u8..).zip(newest.unwrap_or_default()) {
                let frame = [(heard.counter, self.coded(*heard))];
                let pdu = sample::audio(&handles, packet, &frame);
                let pdu = self.notify(line, audio_descriptor, Some(pdu));
                self.owed.extend(pdu.into_iter().map(|pdu| (line, pdu)));
            }
        }
    }

    fn owe_everywhere(&mut self, pdu: &[u8]) {
        let lines = self.lines.iter().map(|(line, _)| (*line, pdu.to_vec()));
        self.owed.extend(lines);
    }

    /// Brings the microphone up to `now`: one frame for every `FRAME` since the mode was set, the oldest dropped past `KEPT_FRAMES` unless a stream has still to send it.
    fn hear(&mut self, now: Instant) {
        let unsent_from = self.run.as_ref().map_or(u32::MAX, |run| run.next);
        let Some(mic) = &mut self.mic else {
            return;
        };
        let elapsed = now.saturating_duration_since(mic.since).as_millis();
        let complete = elapsed.checked_div(FRAME.as_millis()).unwrap_or(0);
        let complete = u32::try_from(complete).unwrap_or(u32::MAX);
        while mic.heard < complete {
            mic.kept.push_back(Heard {
                place: mic.heard,
                counter: self.next_counter,
                marked: self.marked > 0,
            });
            while mic.kept.len() > KEPT_FRAMES
                && mic
                    .kept
                    .front()
                    .is_some_and(|oldest| oldest.place < unsent_from)
            {
                mic.kept.pop_front();
            }
            self.marked = self.marked.saturating_sub(1);
            self.next_counter = self.next_counter.wrapping_add(1);
            mic.heard = mic.heard.saturating_add(1);
        }
    }

    /// The frames still kept from `place` on, oldest first.
    fn kept_from(&self, place: u32) -> Vec<Heard> {
        let kept = self.mic.iter().flat_map(|mic| &mic.kept);
        kept.filter(|heard| heard.place >= place).copied().collect()
    }

    /// The frames of a burst: the newest kept before `place`, with the repeat a test asked for.
    fn opening(&mut self, place: u32) -> Vec<Heard> {
        let (frames, _) = self.burst;
        let frames = usize::try_from(frames).unwrap_or(usize::MAX);
        let kept = self.kept_from(0);
        let before = kept.iter().filter(|heard| heard.place < place).count();
        let newest = kept.get(before.saturating_sub(frames)..before);
        let mut opening = newest.unwrap_or_default().to_vec();
        let at = self.repeat.take().map(usize::from);
        if let Some((at, twice)) = at.and_then(|at| Some((at, *opening.get(at)?))) {
            opening.insert(at, twice);
        }
        opening
    }

    /// How long a stall still holds the next frame of `run` at `now`.
    fn held_back(&self, run: &Run, now: Instant) -> Duration {
        match (self.stall, run.last) {
            (Some(gap), Some(last)) => gap.saturating_sub(now.saturating_duration_since(last)),
            (None, _) | (_, None) => Duration::ZERO,
        }
    }

    /// The bytes of the fixture the microphone coded `heard` to.
    fn coded(&self, heard: Heard) -> &'static [u8] {
        let fixture = if heard.marked { &self.mark } else { &self.tone };
        let place = usize::try_from(heard.place).unwrap_or(usize::MAX);
        let at = place.checked_rem(fixture.len()).unwrap_or(0);
        fixture.get(at).copied().unwrap_or_default()
    }

    /// One audio notification of `frames` on the channel of `run`, less the frames a test loses; none when it loses them all or the host did not ask for audio.
    fn audio(
        &mut self,
        handles: &Handles,
        run: &mut Run,
        frames: &[Heard],
        now: Instant,
    ) -> Option<Vec<u8>> {
        let mut sent = Vec::new();
        for heard in frames {
            if self.lost > 0 {
                self.lost = self.lost.saturating_sub(1);
                continue;
            }
            let mut frame = self.coded(*heard).to_vec();
            if self.garbled > 0 {
                self.garbled = self.garbled.saturating_sub(1);
                frame.splice(..1, [GARBLED_TOC]);
            }
            sent.push((heard.counter, frame));
        }
        if sent.is_empty() {
            return None;
        }
        let sent: Vec<(u16, &[u8])> = sent
            .iter()
            .map(|(counter, frame)| (*counter, frame.as_slice()))
            .collect();
        let pdu = sample::audio(handles, run.packet, &sent);
        run.packet = run.packet.wrapping_add(1);
        let [_, _, audio_descriptor, _] = handles.cccds;
        let sent = self.notify(run.line, audio_descriptor, Some(pdu))?;
        if let Some(last) = run.last.replace(now) {
            let gap = now.saturating_duration_since(last);
            self.largest_gap = self.largest_gap.max(Some(gap));
        }
        Some(sent)
    }

    /// `pdu` for channel `line`, or nothing while the host has not written `descriptor` there.
    fn notify<P: Into<Vec<u8>>>(
        &self,
        line: usize,
        descriptor: u16,
        pdu: Option<P>,
    ) -> Option<Vec<u8>> {
        let open = self.lines.iter().find(|(open, _)| *open == line);
        let listens = open.is_some_and(|(_, descriptors)| descriptors.contains(&descriptor));
        pdu.filter(|_| listens).map(Into::into)
    }
}
