//! What the header bytes, the stamps and the arrival times of one stretch of audio say about its shape.

use std::collections::BTreeSet;

use pods_mic::STAMP_STEP;
use pods_proto::uplink::OPAQUE_LEN;

use super::load::{AudioPacket, Loaded};
use super::stats::{mode, share, wrapping_deltas};

/// An arrival interval over three nominal SDU intervals of 30 ms is a gap.
pub(crate) const LATE_ARRIVAL_NS: u64 = 90_000_000;
/// A one byte counter may miss its step once in this many steps, so a few lost SDUs do not hide it.
const OFF_STEP_ONE_IN: usize = 20;
/// The stamp deltas that are one frame of samples.
const SAMPLE_DELTAS: [u32; 2] = [480, 512];
/// The stamp delta that is one 7.5 ms frame in microseconds.
const MICROSECOND_DELTA: u32 = 7500;
/// A stamp step over half the 32 bit range is a step back, which leaves no stamps out.
const LARGEST_STEP_FORWARD: u32 = i32::MAX.unsigned_abs();

/// What one byte of the 14 opaque header bytes does from SDU to SDU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FieldKind {
    Constant(u8),
    /// Part of a little endian field that wraps at `modulus` and adds `step` in `on_step` of its `steps` from SDU to SDU.
    Counter {
        step: u32,
        modulus: u64,
        on_step: usize,
        steps: usize,
    },
    /// Neither; `distinct` is how many values the byte took, 0 with no audio.
    Varying {
        distinct: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Width {
    One,
    Two,
    Four,
}

impl Width {
    fn bytes(self) -> usize {
        match self {
            Self::One => 1,
            Self::Two => 2,
            Self::Four => 4,
        }
    }

    fn modulus(self) -> u64 {
        match self {
            Self::One => u64::from(u8::MAX).saturating_add(1),
            Self::Two => u64::from(u16::MAX).saturating_add(1),
            Self::Four => u64::from(u32::MAX).saturating_add(1),
        }
    }

    fn read(self, opaque: &[u8; OPAQUE_LEN], at: usize) -> Option<u32> {
        let from = opaque.get(at..)?;
        match self {
            Self::One => from.first().copied().map(u32::from),
            Self::Two => from
                .first_chunk()
                .map(|two| u32::from(u16::from_le_bytes(*two))),
            Self::Four => from.first_chunk().map(|four| u32::from_le_bytes(*four)),
        }
    }

    fn step(self, before: u32, after: u32) -> u32 {
        let step = after.wrapping_sub(before);
        match self {
            Self::One => step & u32::from(u8::MAX),
            Self::Two => step & u32::from(u16::MAX),
            Self::Four => step,
        }
    }

    /// Where a counter this wide is looked for: every byte, every pair, and the four byte words at 0 and 4.
    fn places(self) -> &'static [usize] {
        match self {
            Self::One => &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13],
            Self::Two => &[0, 2, 4, 6, 8, 10, 12],
            Self::Four => &[0, 4],
        }
    }
}

/// A run of opaque bytes that looks like a counter: an observation, never proof that a packet was lost.
#[derive(Debug, Clone, Copy)]
struct Counter {
    at: usize,
    width: Width,
    step: u32,
    /// How many steps from SDU to SDU were `step`.
    on_step: usize,
    steps: usize,
}

impl Counter {
    fn end(&self) -> usize {
        self.at.saturating_add(self.width.bytes())
    }

    fn covers(&self, at: usize) -> bool {
        (self.at..self.end()).contains(&at)
    }
}

fn byte_varies(loaded: &Loaded, at: usize) -> bool {
    let mut values = loaded.audio.iter().map(|sdu| sdu.opaque.get(at));
    let first = values.next();
    values.any(|value| Some(value) != first)
}

fn counter_at(loaded: &Loaded, width: Width, at: usize) -> Option<Counter> {
    let read = |sdu: &AudioPacket| width.read(&sdu.opaque, at);
    let steps: Vec<u32> = loaded
        .audio
        .iter()
        .zip(loaded.audio.iter().skip(1))
        .filter_map(|(before, after)| Some(width.step(read(before)?, read(after)?)))
        .collect();
    let (step, on_step) = mode(steps.iter().copied())?;
    if step == 0 {
        return None;
    }
    let counter = Counter {
        at,
        width,
        step,
        on_step,
        steps: steps.len(),
    };
    let off = steps.len().saturating_sub(on_step);
    let counts = match width {
        Width::One => off.saturating_mul(OFF_STEP_ONE_IN) <= steps.len(),
        // A wide counter must never miss a step and must carry, or one counting byte beside still bytes would pass.
        Width::Two | Width::Four => {
            let mut upper = at.saturating_add(1)..counter.end();
            off == 0 && byte_varies(loaded, at) && upper.any(|byte| byte_varies(loaded, byte))
        }
    };
    counts.then_some(counter)
}

/// Every counter in the opaque bytes, the widest that fits first, none over another.
fn counters(loaded: &Loaded) -> Vec<Counter> {
    let mut found: Vec<Counter> = Vec::new();
    for width in [Width::Four, Width::Two, Width::One] {
        for &at in width.places() {
            let taken = |byte| found.iter().any(|counter| counter.covers(byte));
            let end = at.saturating_add(width.bytes());
            if (at..end).any(taken) {
                continue;
            }
            if let Some(counter) = counter_at(loaded, width, at) {
                found.push(counter);
            }
        }
    }
    found
}

fn field_at(loaded: &Loaded, counters: &[Counter], at: usize) -> FieldKind {
    if let Some(counter) = counters.iter().find(|counter| counter.covers(at)) {
        return FieldKind::Counter {
            step: counter.step,
            modulus: counter.width.modulus(),
            on_step: counter.on_step,
            steps: counter.steps,
        };
    }
    let values: BTreeSet<u8> = loaded
        .audio
        .iter()
        .filter_map(|sdu| sdu.opaque.get(at).copied())
        .collect();
    match (values.first(), values.len()) {
        (Some(&value), 1) => FieldKind::Constant(value),
        (_, distinct) => FieldKind::Varying { distinct },
    }
}

pub(crate) fn opaque_fields(loaded: &Loaded) -> [FieldKind; OPAQUE_LEN] {
    let counters = counters(loaded);
    std::array::from_fn(|at| field_at(loaded, &counters, at))
}

/// How many SDUs hold at opaque byte `at` a little endian `u16` equal to their own length less `less`.
pub(crate) fn length_agrees(loaded: &Loaded, at: usize, less: usize) -> usize {
    let agrees = |sdu: &&AudioPacket| {
        let declared = Width::Two.read(&sdu.opaque, at).map(usize::try_from);
        match (declared, sdu.len.checked_sub(less)) {
            (Some(Ok(declared)), Some(expected)) => declared == expected,
            _ => false,
        }
    };
    loaded.audio.iter().filter(agrees).count()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ModalDelta {
    pub(crate) delta: u32,
    pub(crate) share: f64,
}

/// What a stamp counts, judged by its modal step from unit to unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StampUnit {
    Samples,
    Microseconds,
    UplinkTicks,
    /// No known unit steps by this much.
    Unknown(u32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StampReport {
    /// From each unit to the next, across SDUs too; `None` under two units.
    pub(crate) per_unit: Option<ModalDelta>,
    /// From the first unit of each SDU to the first of the next.
    pub(crate) per_sdu: Option<ModalDelta>,
}

impl StampReport {
    pub(crate) fn implied(&self) -> Option<StampUnit> {
        self.per_unit.map(|modal| implied_unit(modal.delta))
    }

    /// The step a stamp break is judged against; `None` when the stamps do not step from unit to unit, so they show no gap.
    pub(crate) fn step(&self) -> Option<u32> {
        let modal = self.per_unit.map(|modal| modal.delta);
        modal.filter(|&delta| delta != 0)
    }
}

pub(crate) fn implied_unit(delta: u32) -> StampUnit {
    if SAMPLE_DELTAS.contains(&delta) {
        StampUnit::Samples
    } else if delta == MICROSECOND_DELTA {
        StampUnit::Microseconds
    } else if delta == STAMP_STEP {
        StampUnit::UplinkTicks
    } else {
        StampUnit::Unknown(delta)
    }
}

fn modal_delta(stamps: &[u32]) -> Option<ModalDelta> {
    let deltas = wrapping_deltas(stamps);
    let (delta, seen) = mode(deltas.iter().copied())?;
    let share = share(seen, deltas.len())?;
    Some(ModalDelta { delta, share })
}

pub(crate) fn stamps(loaded: &Loaded) -> StampReport {
    let per_unit: Vec<u32> = loaded.units().map(|unit| unit.stamp).collect();
    let per_sdu: Vec<u32> = loaded
        .audio
        .iter()
        .filter_map(|sdu| sdu.units.first())
        .map(|unit| unit.stamp)
        .collect();
    StampReport {
        per_unit: modal_delta(&per_unit),
        per_sdu: modal_delta(&per_sdu),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StampBreak {
    /// The step is several modal steps: the stamps `first` to `last`, a modal step apart, never came.
    Missing { first: u32, last: u32 },
    /// The step is no whole number of modal steps forward: `delta` over half the 32 bit range is a step back.
    Irregular { delta: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Gap {
    /// The SDU the gap ends at, by its place among the loaded records, counted from 0.
    pub(crate) sdu: usize,
    pub(crate) at_ns: u64,
    /// The arrival interval before it, when over `LATE_ARRIVAL_NS`.
    pub(crate) late_ns: Option<u64>,
    /// The first step off the modal one among the stamps up to the end of the SDU.
    pub(crate) stamps: Option<StampBreak>,
}

fn broken(before: u32, after: u32, modal: u32) -> StampBreak {
    let delta = after.wrapping_sub(before);
    let forward = (modal..=LARGEST_STEP_FORWARD).contains(&delta);
    if forward && delta.checked_rem(modal) == Some(0) {
        StampBreak::Missing {
            first: before.wrapping_add(modal),
            last: after.wrapping_sub(modal),
        }
    } else {
        StampBreak::Irregular { delta }
    }
}

/// The first step off `modal` from `last`, the stamp before the SDU, through the stamps of `sdu`.
fn stamp_break(last: Option<u32>, sdu: &AudioPacket, modal: u32) -> Option<StampBreak> {
    let mut before = last;
    for unit in &sdu.units {
        if let Some(before) = before
            && unit.stamp.wrapping_sub(before) != modal
        {
            return Some(broken(before, unit.stamp, modal));
        }
        before = Some(unit.stamp);
    }
    None
}

/// Every gap, one for each SDU that ends one: a late arrival or a break in the stamps, never a header byte.
pub(crate) fn gaps(loaded: &Loaded) -> Vec<Gap> {
    let modal = stamps(loaded).step();
    let mut found = Vec::new();
    let mut before: Option<&AudioPacket> = None;
    let mut last_stamp: Option<u32> = None;
    for sdu in &loaded.audio {
        let interval_ns = before.map(|before| sdu.at_ns.saturating_sub(before.at_ns));
        let gap = Gap {
            sdu: sdu.record,
            at_ns: sdu.at_ns,
            late_ns: interval_ns.filter(|&interval_ns| interval_ns > LATE_ARRIVAL_NS),
            stamps: modal.and_then(|modal| stamp_break(last_stamp, sdu, modal)),
        };
        if gap.late_ns.is_some() || gap.stamps.is_some() {
            found.push(gap);
        }
        before = Some(sdu);
        last_stamp = sdu.units.last().map(|unit| unit.stamp).or(last_stamp);
    }
    found
}

#[cfg(test)]
mod tests;
