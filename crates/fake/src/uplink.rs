//! The pods' side of the microphone uplink: what a START and a STOP are answered with and when each audio packet is due, with no socket or thread.

use std::collections::VecDeque;
use std::mem;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pods_samples::uplink::{
    STAMP_STEP, START, STOP, audio_sdu, session_marker, state_marker, tone_unit,
};

use crate::serve::Shared;

/// The time between two audio packets, which is the audio one packet holds.
pub const PERIOD: Duration = Duration::from_millis(30);
pub const UNITS_PER_PACKET: u32 = 4;
/// The packets a START is followed by while no bud is worn.
pub const NOT_WORN_PACKETS: u32 = 5;
/// The spectral line of the tone every unit holds, and of the second tone a marked packet holds.
pub const TONE_LINE: u16 = 15;
pub const MARK_LINE: u16 = 30;
/// The scale factor of the units until a test sets another: far above silence and far below full scale.
pub const DEFAULT_LEVEL: u8 = 176;
/// The audio packets that still go out after a STOP.
const STOP_TAIL_PACKETS: u32 = 2;

/// The test's hand on the uplink of one pair of pods; every clone is the same uplink.
#[derive(Debug, Clone)]
pub struct Uplink {
    shared: Arc<Shared>,
}

impl Uplink {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    /// While held a START is heard and not answered until `release_start`.
    pub fn hold_starts(&self, hold: bool) {
        self.shared.uplink(|uplink| uplink.hold_starts = hold);
    }

    /// Answers the oldest held START as one that just came: both running markers, then audio.
    pub fn release_start(&self) {
        self.shared
            .uplink(|uplink| uplink.release_start(Instant::now()));
    }

    /// While held a STOP is heard and the audio goes on until `release_stop`.
    pub fn hold_stops(&self, hold: bool) {
        self.shared.uplink(|uplink| uplink.hold_stops = hold);
    }

    /// Answers a held STOP: two more packets, then both stopped markers.
    pub fn release_stop(&self) {
        self.shared.uplink(Machine::release_stop);
    }

    /// What the pods do in a headset profile: a START is heard and nothing is said.
    pub fn never_answer(&self, never: bool) {
        self.shared.uplink(|uplink| uplink.never_answer = never);
    }

    /// Another host took the pods: both stopped markers with no STOP heard.
    pub fn stop_unasked(&self) {
        self.shared.uplink(Machine::stop_unasked);
    }

    /// The next `packets` packets hold the second tone.
    pub fn mark(&self, packets: u32) {
        self.shared.uplink(|uplink| uplink.marked = packets);
    }

    /// The scale factor of the units from now on, 1.5 dB a step.
    pub fn set_level(&self, level: u8) {
        self.shared.uplink(|uplink| uplink.level = level);
    }

    /// Holds the next packet until `gap` after the one before it; the packets that come due meanwhile go out with it, each with its own stamp.
    pub fn stall(&self, gap: Duration) {
        self.shared.uplink(|uplink| uplink.stall = Some(gap));
    }

    /// The next `packets` packets are made and never sent, so the stamps pass over them.
    pub fn lose(&self, packets: u32) {
        self.shared.uplink(|uplink| uplink.lost = packets);
    }

    /// From the answer to a START until both stopped markers have gone out.
    #[must_use]
    pub fn running(&self) -> bool {
        self.shared.uplink(|uplink| uplink.run.is_some())
    }

    /// The audio packets the pods made on their own clock since they were made, read by the host or still queued for it.
    #[must_use]
    pub fn sent_packets(&self) -> u64 {
        self.shared.uplink(|uplink| uplink.sent_packets)
    }

    /// The longest time between two audio packets the pods made since the last START was answered; a host that reads late shows on its own side, not here.
    #[must_use]
    pub fn largest_gap(&self) -> Duration {
        self.shared.uplink(|uplink| uplink.largest_gap)
    }
}

#[derive(Debug)]
pub(crate) struct Machine {
    hold_starts: bool,
    hold_stops: bool,
    never_answer: bool,
    /// The channel of each START heard and not answered, oldest first.
    held_starts: VecDeque<usize>,
    level: u8,
    marked: u32,
    lost: u32,
    stall: Option<Duration>,
    run: Option<Run>,
    sent_packets: u64,
    largest_gap: Duration,
}

/// The uplink from the answer to a START until the stopped markers, on the channel the START came on.
#[derive(Debug)]
struct Run {
    line: usize,
    started: Instant,
    /// Whether the running markers have gone out.
    announced: bool,
    /// The next place of the schedule, which counts on where no packet goes out.
    next: u32,
    /// When the last audio packet went out.
    last: Option<Instant>,
    stop_held: bool,
    /// The place the stopped markers go out at, in place of its packet.
    ends_at: Option<u32>,
}

impl Run {
    /// The first call sets the end; a run that is ending already keeps its end.
    fn end_after(&mut self, packets: u32) {
        self.ends_at
            .get_or_insert(self.next.saturating_add(packets));
    }

    fn ended(&self) -> bool {
        self.ends_at == Some(self.next)
    }
}

impl Machine {
    pub(crate) fn new() -> Self {
        Self {
            hold_starts: false,
            hold_stops: false,
            never_answer: false,
            held_starts: VecDeque::new(),
            level: DEFAULT_LEVEL,
            marked: 0,
            lost: 0,
            stall: None,
            run: None,
            sent_packets: 0,
            largest_gap: Duration::ZERO,
        }
    }

    /// A frame the host sent on channel `line` at `now`; only a START and a STOP are the uplink's.
    pub(crate) fn on_host(&mut self, line: usize, sdu: &[u8], now: Instant) {
        let hold_stops = self.hold_stops;
        if sdu == START {
            // corner: assumed until measured on hardware, a START while the uplink runs changes nothing
            if self.never_answer || self.run.is_some() {
            } else if self.hold_starts {
                self.held_starts.push_back(line);
            } else {
                self.start(line, now);
            }
        } else if sdu == STOP
            // corner: assumed until measured on hardware, a STOP while the uplink does not run is answered with nothing
            && let Some(run) = self.run.as_mut().filter(|run| run.line == line)
        {
            if hold_stops {
                run.stop_held = true;
            } else {
                run.end_after(STOP_TAIL_PACKETS);
            }
        }
    }

    /// Channel `line` ended: a run on it ends unsaid, and its held STARTs are gone.
    pub(crate) fn closed(&mut self, line: usize) {
        if self.run.as_ref().is_some_and(|run| run.line == line) {
            self.run = None;
        }
        self.held_starts.retain(|held| *held != line);
    }

    /// What is due on channel `line` at `now`, oldest first: the markers of a state just entered and every audio packet whose time has come.
    pub(crate) fn due(&mut self, line: usize, worn: bool, now: Instant) -> Vec<Vec<u8>> {
        let mut due = Vec::new();
        let Some(mut run) = self.run.take_if(|run| run.line == line) else {
            return due;
        };
        if !mem::replace(&mut run.announced, true) {
            due.extend(markers(true));
        }
        while !run.ended() && self.wait_of(&run, now).is_zero() {
            due.extend(self.packet(&mut run, worn, now));
        }
        if run.ended() {
            due.extend(markers(false));
        } else {
            self.run = Some(run);
        }
        due
    }

    /// How long channel `line` may wait at `now` before something is due on it; none while the uplink does not run on it.
    pub(crate) fn wait(&self, line: usize, now: Instant) -> Option<Duration> {
        let run = self.run.as_ref().filter(|run| run.line == line)?;
        let owed = !run.announced || run.ended();
        Some(if owed {
            Duration::ZERO
        } else {
            self.wait_of(run, now)
        })
    }

    fn start(&mut self, line: usize, now: Instant) {
        self.largest_gap = Duration::ZERO;
        self.run = Some(Run {
            line,
            started: now,
            announced: false,
            next: 0,
            last: None,
            stop_held: false,
            ends_at: None,
        });
    }

    fn release_start(&mut self, now: Instant) {
        if let Some(line) = self.held_starts.pop_front()
            && self.run.is_none()
        {
            self.start(line, now);
        }
    }

    fn release_stop(&mut self) {
        if let Some(run) = &mut self.run
            && mem::take(&mut run.stop_held)
        {
            run.end_after(STOP_TAIL_PACKETS);
        }
    }

    fn stop_unasked(&mut self) {
        if let Some(run) = &mut self.run {
            run.end_after(0);
        }
    }

    /// How long from `now` until the next place of the schedule is due: a period after the place before it, and no sooner than a stall holds it.
    fn wait_of(&self, run: &Run, now: Instant) -> Duration {
        let due = PERIOD.saturating_mul(run.next.saturating_add(1));
        let on_schedule = due.saturating_sub(now.saturating_duration_since(run.started));
        let last = run.last.unwrap_or(run.started);
        let held = self.stall.map_or(Duration::ZERO, |gap| {
            gap.saturating_sub(now.saturating_duration_since(last))
        });
        on_schedule.max(held)
    }

    /// The audio packet of the next place of the schedule; none where the pods make none or the test loses it.
    fn packet(&mut self, run: &mut Run, worn: bool, now: Instant) -> Option<Vec<u8>> {
        let slot = run.next;
        run.next = slot.saturating_add(1);
        self.stall = None;
        // corner: assumed until measured on hardware, the five packets count from the START however the buds move after it
        if !worn && slot >= NOT_WORN_PACKETS {
            return None;
        }
        let line = if self.marked > 0 {
            self.marked = self.marked.saturating_sub(1);
            MARK_LINE
        } else {
            TONE_LINE
        };
        if self.lost > 0 {
            self.lost = self.lost.saturating_sub(1);
            return None;
        }
        if let Some(last) = run.last.replace(now) {
            let gap = now.saturating_duration_since(last);
            self.largest_gap = self.largest_gap.max(gap);
        }
        self.sent_packets = self.sent_packets.saturating_add(1);
        let units: Vec<Vec<u8>> = (0..UNITS_PER_PACKET)
            .map(|_| tone_unit(line, self.level))
            .collect();
        let stamp = slot.wrapping_mul(UNITS_PER_PACKET.wrapping_mul(STAMP_STEP));
        Some(audio_sdu(stamp, &units))
    }
}

// corner: assumed until measured on hardware, the state marker comes before the session marker
fn markers(running: bool) -> [Vec<u8>; 2] {
    [
        state_marker(running).to_vec(),
        session_marker(running).to_vec(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: usize = 3;
    const NOTHING: [Vec<u8>; 0] = [];

    fn ms(count: u64) -> Duration {
        Duration::from_millis(count)
    }

    /// A machine whose uplink a START started on `LINE` at the instant returned, its running markers taken.
    fn running() -> (Machine, Instant) {
        let mut machine = Machine::new();
        let start = Instant::now();
        machine.on_host(LINE, &START, start);
        assert_eq!(machine.due(LINE, true, start), markers(true));
        (machine, start)
    }

    fn packet_of(slot: u32, line: u16, level: u8) -> Vec<u8> {
        let units: Vec<Vec<u8>> = (0..UNITS_PER_PACKET)
            .map(|_| tone_unit(line, level))
            .collect();
        audio_sdu(
            slot.wrapping_mul(UNITS_PER_PACKET.wrapping_mul(STAMP_STEP)),
            &units,
        )
    }

    fn tone(slot: u32) -> Vec<u8> {
        packet_of(slot, TONE_LINE, DEFAULT_LEVEL)
    }

    #[test]
    fn a_packet_is_due_each_period_from_the_start_and_never_before() {
        let (mut machine, start) = running();
        assert_eq!(machine.wait(LINE, start), Some(PERIOD));
        assert_eq!(machine.due(LINE, true, start + ms(29)), NOTHING);
        assert_eq!(machine.due(LINE, true, start + ms(30)), [tone(0)]);
        // A START while the uplink runs changes nothing.
        machine.on_host(LINE, &START, start + ms(40));
        // A late turn sends every packet that came due, and the next keeps its own time.
        assert_eq!(machine.due(LINE, true, start + ms(95)), [tone(1), tone(2)]);
        assert_eq!(machine.wait(LINE, start + ms(95)), Some(ms(25)));
        assert_eq!((machine.sent_packets, machine.largest_gap), (3, ms(65)));
        assert_eq!(machine.wait(LINE + 1, start), None);
        assert_eq!(machine.due(LINE + 1, true, start + ms(500)), NOTHING);
    }

    #[test]
    fn a_stop_is_followed_by_two_packets_then_both_stopped_markers() {
        let (mut machine, start) = running();
        assert_eq!(machine.due(LINE, true, start + ms(30)), [tone(0)]);
        machine.on_host(LINE + 1, &STOP, start + ms(35));
        assert_eq!(machine.due(LINE, true, start + ms(60)), [tone(1)]);
        machine.on_host(LINE, &STOP, start + ms(65));
        assert_eq!(machine.run.as_ref().and_then(|run| run.ends_at), Some(4));
        assert_eq!(machine.due(LINE, true, start + ms(90)), [tone(2)]);
        // The run that is ending keeps its end, whatever comes after the STOP.
        machine.on_host(LINE, &STOP, start + ms(95));
        machine.stop_unasked();
        machine.on_host(LINE, &START, start + ms(95));
        let [state, session] = markers(false);
        let end = [tone(3), state, session];
        assert_eq!(machine.due(LINE, true, start + ms(120)), end);
        assert!(machine.run.is_none());
        machine.on_host(LINE, &STOP, start + ms(130));
        assert_eq!(machine.due(LINE, true, start + ms(200)), NOTHING);
    }

    #[test]
    fn a_held_start_and_a_held_stop_wait_for_their_release() {
        let mut machine = Machine::new();
        let start = Instant::now();
        (machine.hold_starts, machine.hold_stops) = (true, true);
        machine.on_host(LINE, &START, start);
        assert!(machine.run.is_none());
        assert_eq!(machine.due(LINE, true, start + ms(200)), NOTHING);
        machine.release_start(start + ms(300));
        // With nothing held a release changes nothing.
        machine.release_start(start + ms(310));
        machine.release_stop();
        assert_eq!(machine.due(LINE, true, start + ms(300)), markers(true));
        assert_eq!(machine.due(LINE, true, start + ms(330)), [tone(0)]);
        machine.on_host(LINE, &STOP, start + ms(340));
        assert_eq!(machine.due(LINE, true, start + ms(390)), [tone(1), tone(2)]);
        machine.release_stop();
        let [state, session] = markers(false);
        let end = [tone(3), tone(4), state, session];
        assert_eq!(machine.due(LINE, true, start + ms(450)), end);
    }

    #[test]
    fn never_answer_and_a_closed_channel_leave_no_run() {
        let mut machine = Machine::new();
        machine.never_answer = true;
        machine.on_host(LINE, &START, Instant::now());
        assert!(machine.run.is_none() && machine.held_starts.is_empty());

        let (mut machine, start) = running();
        machine.hold_starts = true;
        machine.closed(LINE + 1);
        assert!(machine.run.is_some());
        machine.closed(LINE);
        assert_eq!(machine.due(LINE, true, start + ms(60)), NOTHING);
        machine.on_host(LINE, &START, start);
        machine.closed(LINE);
        machine.release_start(start);
        assert!(machine.run.is_none());
    }

    #[test]
    fn a_stall_holds_one_packet_and_the_schedule_goes_on() {
        let (mut machine, start) = running();
        assert_eq!(machine.due(LINE, true, start + ms(30)), [tone(0)]);
        machine.stall = Some(ms(100));
        assert_eq!(machine.wait(LINE, start + ms(60)), Some(ms(70)));
        assert_eq!(machine.due(LINE, true, start + ms(129)), NOTHING);
        let burst = [tone(1), tone(2), tone(3)];
        assert_eq!(machine.due(LINE, true, start + ms(130)), burst);
        assert_eq!(machine.wait(LINE, start + ms(130)), Some(ms(20)));
        assert_eq!(machine.due(LINE, true, start + ms(150)), [tone(4)]);
        assert_eq!(machine.largest_gap, ms(100));
    }

    #[test]
    fn with_no_bud_worn_five_packets_go_out_and_the_schedule_counts_on() {
        let (mut machine, start) = running();
        let five: Vec<Vec<u8>> = (0..NOT_WORN_PACKETS).map(tone).collect();
        assert_eq!(machine.due(LINE, false, start + ms(300)), five);
        assert_eq!(machine.due(LINE, true, start + ms(330)), [tone(10)]);
        machine.on_host(LINE, &STOP, start + ms(340));
        assert_eq!(machine.due(LINE, false, start + ms(390)), markers(false));
        assert_eq!(machine.sent_packets, 6);
    }

    #[test]
    fn a_lost_packet_takes_its_stamp_and_its_mark_with_it() {
        let (mut machine, start) = running();
        (machine.lost, machine.marked, machine.level) = (1, 2, 170);
        let after = [packet_of(1, MARK_LINE, 170), packet_of(2, TONE_LINE, 170)];
        assert_eq!(machine.due(LINE, true, start + ms(90)), after);
        assert_eq!(machine.sent_packets, 2);
    }

    #[test]
    fn a_stop_nobody_asked_for_ends_the_run_at_once() {
        let (mut machine, start) = running();
        machine.stop_unasked();
        assert_eq!(machine.wait(LINE, start), Some(Duration::ZERO));
        assert_eq!(machine.due(LINE, true, start), markers(false));
        assert!(machine.run.is_none());
    }
}
