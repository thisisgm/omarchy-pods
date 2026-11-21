//! The rows of one start mark: its trial, its rates, the shape of its stream, its lead and the audio after its stop.

use pods_proto::sdulog::Record;
use pods_proto::uplink::{AUDIO_HEADER_LEN, OPAQUE_LEN};

use super::super::bands::{
    LAST_BAND, LINES_PER_CLOCK, TABLE_CLOCK_HZ, band_hz, band_line, bandwidth,
};
use super::super::census::{Trial, trials};
use super::super::input::Mark;
use super::super::lead::{Arrival, LEAD_MIN_PACKETS, LEAD_WINDOW_NS, lead};
use super::super::load::{Loaded, Window, load, segment, tails, windows};
use super::super::rates::{
    ClockEstimate, THIN_MIN_SDUS, THIN_MIN_SPAN_NS, Thin, bitrate, cadence, clock,
};
use super::super::shape::{
    FieldKind, Gap, LATE_ARRIVAL_NS, ModalDelta, StampBreak, StampReport, StampUnit, gaps,
    length_agrees, opaque_fields, stamps,
};
use super::super::stats::{NS_PER_MS, NS_PER_SECOND, millis, seconds, share};
use super::{BYTES, MS, NONE, NOTE_SEPARATOR, PACKETS, Row, UNITS};

/// Samples in one unit, which turns the unit clock into a sample clock; no packet states it, so every row scaled by it says it is assumed.
const SAMPLES_PER_UNIT: u32 = 480;
/// The duration of one unit when the segment is too thin to give its own.
const NOMINAL_FRAME_NS: f64 = 7_500_000.0;
/// The sample clocks the measured one is set against.
const REFERENCE_HZ: [u32; 2] = [64_000, 48_000];
/// Only this many gaps of a segment get a row of their own.
const GAPS_LISTED: usize = 20;
/// The header byte the opaque bytes start at.
const OPAQUE_AT: usize = AUDIO_HEADER_LEN - OPAQUE_LEN;
/// The last header byte.
const HEADER_END: usize = AUDIO_HEADER_LEN - 1;
/// The opaque bytes checked as a little endian length, each with the bytes of the SDU it leaves out.
const LENGTH_FIELDS: [(usize, usize); 2] = [(0, 10), (6, 16)];
/// What a rate row reads when the segment is too thin for it.
const REFUSED: &str = "refused";
const HZ: &str = "Hz";
const BIT_PER_SECOND: &str = "bit/s";
const STAMP_TICKS: &str = "stamp ticks";
/// What the gaps row says when the stamps have no step a break could be judged against.
const STAMPS_UNCHECKED: &str = "stamps not checked, no modal step from unit to unit";

/// The audio of one start mark, cut at the mark that ends it.
struct Segment {
    /// The first item of every note: which start mark the row belongs to.
    label: String,
    start_ns: u64,
    loaded: Loaded,
    stamps: StampReport,
    gaps: Vec<Gap>,
    clock: Result<ClockEstimate, Thin>,
    /// `None` when no stop mark ends the start.
    tail: Option<Tail>,
}

/// The audio the pods went on sending after the stop mark that ends a start, up to the next start mark.
struct Tail {
    sdus: usize,
    /// From the stop mark to the last of them; `None` when none came.
    last_after_ns: Option<u64>,
}

fn tail_in(records: &[Record], after_stop: Window) -> Tail {
    let audio = load(segment(records, after_stop)).audio;
    let since_stop = |at_ns: u64| at_ns.saturating_sub(after_stop.start_ns);
    Tail {
        sdus: audio.len(),
        last_after_ns: audio.last().map(|sdu| since_stop(sdu.at_ns)),
    }
}

/// The rows of every start mark, in the order of the marks.
pub(super) fn start_rows(records: &[Record], marks: &[Mark], whole: &Loaded) -> Vec<Row> {
    let trials = trials(whole, marks);
    let starts = windows(marks).into_iter().zip(tails(marks)).zip(&trials);
    let mut rows = Vec::new();
    for (((window, after_stop), trial), ordinal) in starts.zip(1_usize..) {
        let loaded = load(segment(records, window));
        let start = Segment {
            label: format!("start {ordinal}"),
            start_ns: window.start_ns,
            stamps: stamps(&loaded),
            gaps: gaps(&loaded),
            clock: clock(&loaded, SAMPLES_PER_UNIT),
            loaded,
            tail: after_stop.map(|after_stop| tail_in(records, after_stop)),
        };
        rows.extend(start.trial_rows(trial));
        rows.extend(start.cadence_rows());
        rows.extend(start.clock_rows());
        rows.extend(start.bitrate_rows());
        rows.extend(start.stamp_rows());
        rows.push(start.opaque_row());
        rows.extend(start.length_rows());
        rows.extend(start.gap_rows());
        rows.extend(start.band_rows());
        rows.extend(start.lead_rows());
        rows.extend(start.tail_rows());
    }
    rows
}

fn thin_note(thin: &Thin) -> String {
    format!(
        "{} audio SDUs over {:.3} s; needs {THIN_MIN_SDUS} over {:.0} s",
        thin.sdus,
        seconds(thin.span_ns),
        seconds(THIN_MIN_SPAN_NS)
    )
}

/// What every figure scaled by the unit length says: the length is not read from the stream.
fn assumed_unit(clock: &ClockEstimate) -> String {
    let samples = clock.samples_per_unit;
    format!("{samples} samples per unit assumed, not measured")
}

fn field_token(field: FieldKind) -> String {
    match field {
        FieldKind::Constant(value) => format!("constant(0x{value:02x})"),
        FieldKind::Counter {
            step,
            modulus,
            on_step,
            steps,
        } => {
            // A counter is found only among steps, so the fallback is never printed.
            let share = share(on_step, steps).unwrap_or(0.0);
            format!("counter(step={step},modulus={modulus},share={share:.4})")
        }
        FieldKind::Varying { distinct } => format!("varying(distinct={distinct})"),
    }
}

impl Segment {
    /// The label, then every item that is not empty.
    fn note(&self, items: &[&str]) -> String {
        let said = items.iter().copied().filter(|item| !item.is_empty());
        let all: Vec<&str> = std::iter::once(self.label.as_str()).chain(said).collect();
        all.join(NOTE_SEPARATOR)
    }

    /// A row thin data refuses: the figure `show` gives with its derivation, or `refused` with what the segment held.
    fn rate<T>(
        &self,
        key: &str,
        unit: &'static str,
        figure: &Result<T, Thin>,
        show: impl Fn(&T) -> (String, String),
    ) -> Row {
        match figure {
            Ok(figure) => {
                let (value, derivation) = show(figure);
                // A lost SDU bends every rate of its segment, so each of them says the segment has one.
                let gap = if self.gaps.is_empty() { "" } else { "gap" };
                Row::new(key, &value, unit, &self.note(&[&derivation, gap]))
            }
            Err(thin) => Row::new(key, &REFUSED, unit, &self.note(&[&thin_note(thin)])),
        }
    }

    fn trial_rows(&self, trial: &Trial) -> [Row; 4] {
        let ms = |after_ns: Option<u64>| {
            after_ns.map_or_else(|| NONE.to_owned(), |ns| format!("{:.3}", millis(ns)))
        };
        let audio = &self.loaded.audio;
        let span_ns = match (audio.first(), audio.last()) {
            (Some(first), Some(last)) => last.at_ns.saturating_sub(first.at_ns),
            _ => 0,
        };
        [
            Row::new(
                "trial_control_ms",
                &ms(trial.control_after_ns),
                MS,
                &self.note(&["start mark to the first control packet from the pods"]),
            ),
            Row::new(
                "trial_audio_ms",
                &ms(trial.audio_after_ns),
                MS,
                &self.note(&["start mark to the first audio SDU"]),
            ),
            Row::new("segment_audio_sdus", &audio.len(), PACKETS, &self.note(&[])),
            Row::new(
                "segment_span_s",
                &format!("{:.3}", seconds(span_ns)),
                "s",
                &self.note(&["first to last audio SDU"]),
            ),
        ]
    }

    fn cadence_rows(&self) -> [Row; 5] {
        let cadence = cadence(&self.loaded);
        let interval = |ns: u64| (format!("{:.3}", millis(ns)), String::new());
        [
            self.rate("sdus_per_second", "SDUs/s", &cadence, |cadence| {
                let derivation = "arrival intervals over the first to last span";
                (
                    format!("{:.6}", cadence.sdus_per_second),
                    derivation.to_owned(),
                )
            }),
            self.rate("units_per_sdu", UNITS, &cadence, |cadence| {
                let derivation = format!("modal; share {:.4}", cadence.units_per_sdu_share);
                (cadence.units_per_sdu.to_string(), derivation)
            }),
            self.rate("sdu_interval_ms_min", MS, &cadence, |cadence| {
                interval(cadence.interval_min_ns)
            }),
            self.rate("sdu_interval_ms_median", MS, &cadence, |cadence| {
                interval(cadence.interval_median_ns)
            }),
            self.rate("sdu_interval_ms_max", MS, &cadence, |cadence| {
                interval(cadence.interval_max_ns)
            }),
        ]
    }

    fn clock_rows(&self) -> Vec<Row> {
        let clock = &self.clock;
        let flagged = |clock: &ClockEstimate, derivation: String| {
            if clock.disagree() {
                format!("{derivation}{NOTE_SEPARATOR}disagree")
            } else {
                derivation
            }
        };
        let scaled = |clock: &ClockEstimate, derivation: String| {
            let assumed = assumed_unit(clock);
            flagged(clock, format!("{derivation}{NOTE_SEPARATOR}{assumed}"))
        };
        let mut rows = vec![
            self.rate("clock_units_per_second", "units/s", clock, |clock| {
                let ratio = clock.ratio;
                let derivation = format!("least squares slope; first to last ratio {ratio:.6}");
                (
                    format!("{:.6}", clock.units_per_second),
                    flagged(clock, derivation),
                )
            }),
            self.rate("clock_hz", HZ, clock, |clock| {
                let derivation = format!("slope x {} samples per unit", clock.samples_per_unit);
                (format!("{:.3}", clock.hz()), scaled(clock, derivation))
            }),
            self.rate("clock_hz_low", HZ, clock, |clock| {
                let derivation = "low end of the 95 percent interval of the slope".to_owned();
                (format!("{:.3}", clock.hz_low()), scaled(clock, derivation))
            }),
            self.rate("clock_hz_high", HZ, clock, |clock| {
                let derivation = "high end of the 95 percent interval of the slope".to_owned();
                (format!("{:.3}", clock.hz_high()), scaled(clock, derivation))
            }),
        ];
        for reference_hz in REFERENCE_HZ {
            let key = format!("clock_ppm_vs_{reference_hz}");
            rows.push(self.rate(&key, "ppm", clock, |clock| {
                let derivation = format!("clock_hz against {reference_hz} Hz");
                let ppm = clock.ppm_vs(f64::from(reference_hz));
                (format!("{ppm:.1}"), scaled(clock, derivation))
            }));
        }
        rows
    }

    fn bitrate_rows(&self) -> [Row; 4] {
        let bitrate = bitrate(&self.loaded);
        let bytes = |length: Option<usize>| {
            length.map_or_else(|| NONE.to_owned(), |length| length.to_string())
        };
        [
            self.rate(
                "audio_bits_per_second",
                BIT_PER_SECOND,
                &bitrate,
                |bitrate| {
                    let derivation = "coded unit bytes x 8 over the first to last span";
                    (
                        format!("{:.1}", bitrate.audio_bits_per_second),
                        derivation.to_owned(),
                    )
                },
            ),
            self.rate("sdu_bits_per_second", BIT_PER_SECOND, &bitrate, |bitrate| {
                let derivation = "whole SDU bytes x 8 over the first to last span";
                (
                    format!("{:.1}", bitrate.sdu_bits_per_second),
                    derivation.to_owned(),
                )
            }),
            self.rate("unit_bytes_min", BYTES, &bitrate, |bitrate| {
                let shortest = bitrate.unit_bytes.map(|(shortest, _)| shortest);
                (bytes(shortest), "shortest unit".to_owned())
            }),
            self.rate("unit_bytes_max", BYTES, &bitrate, |bitrate| {
                let longest = bitrate.unit_bytes.map(|(_, longest)| longest);
                (bytes(longest), "longest unit".to_owned())
            }),
        ]
    }

    fn stamp_rows(&self) -> [Row; 3] {
        let delta_row = |key: &str, modal: Option<ModalDelta>| match modal {
            Some(modal) => {
                let derivation = format!("modal; share {:.4}", modal.share);
                Row::new(key, &modal.delta, STAMP_TICKS, &self.note(&[&derivation]))
            }
            None => Row::new(key, &NONE, STAMP_TICKS, &self.note(&[])),
        };
        let unit = match (self.stamps.implied(), self.stamps.per_unit) {
            (Some(unit), Some(modal)) => {
                let name = match unit {
                    StampUnit::Samples => "samples",
                    StampUnit::Microseconds => "microseconds",
                    StampUnit::Unknown(_) => "unknown",
                };
                let derivation = format!("modal delta per unit {}", modal.delta);
                Row::new("stamp_unit", &name, "name", &self.note(&[&derivation]))
            }
            _ => Row::new("stamp_unit", &NONE, "name", &self.note(&[])),
        };
        [
            delta_row("stamp_delta_per_unit", self.stamps.per_unit),
            delta_row("stamp_delta_per_sdu", self.stamps.per_sdu),
            unit,
        ]
    }

    fn opaque_row(&self) -> Row {
        let tokens = opaque_fields(&self.loaded).map(field_token);
        let sdus = self.loaded.audio.len();
        let order = format!("header bytes {OPAQUE_AT} to {HEADER_END} in order");
        let seen = "what each byte did from SDU to SDU; a counter here is never a gap";
        Row::new(
            "opaque_fields",
            &tokens.join(" "),
            "kind per byte",
            &self.note(&[&order, &format!("{sdus} SDUs"), seen]),
        )
    }

    /// One row for each header field checked as a length: how many SDUs it agrees with.
    fn length_rows(&self) -> [Row; 2] {
        let sdus = self.loaded.audio.len();
        LENGTH_FIELDS.map(|(at, less)| {
            let header_byte = OPAQUE_AT.saturating_add(at);
            let agree = length_agrees(&self.loaded, at, less);
            let rule = format!(
                "of {sdus} SDUs; little endian u16 from header byte {header_byte} against the SDU length less {less}"
            );
            let key = format!("header_length_at_{header_byte}");
            Row::new(&key, &agree, PACKETS, &self.note(&[&rule]))
        })
    }

    fn gap_rows(&self) -> Vec<Row> {
        let late_ms = millis(LATE_ARRIVAL_NS);
        let rule = match self.stamps.step() {
            Some(_) => format!("arrival over {late_ms:.0} ms or a stamp delta off the modal"),
            None => format!("arrival over {late_ms:.0} ms{NOTE_SEPARATOR}{STAMPS_UNCHECKED}"),
        };
        let cut = if self.gaps.len() > GAPS_LISTED {
            format!("first {GAPS_LISTED} listed")
        } else {
            String::new()
        };
        let count = Row::new("gaps", &self.gaps.len(), "gaps", &self.note(&[&rule, &cut]));
        let listed = self.gaps.iter().take(GAPS_LISTED);
        std::iter::once(count)
            .chain(listed.map(|gap| self.gap_row(gap)))
            .collect()
    }

    /// One gap: when its SDU arrived after the start mark, and everything that shows the gap.
    fn gap_row(&self, gap: &Gap) -> Row {
        let after_ms = millis(gap.at_ns.saturating_sub(self.start_ns));
        let late = gap.late_ns.map_or_else(String::new, |late_ns| {
            format!("late {:.3} ms", millis(late_ns))
        });
        // A stamp break is found only against a modal delta, so the fallback is never printed.
        let due = self.stamps.per_unit.map_or(0, |modal| modal.delta);
        let stamps = match gap.stamps {
            Some(StampBreak::Missing { first, last }) => {
                format!("stamps {first} to {last} missing")
            }
            Some(StampBreak::Irregular { delta }) => {
                format!("stamp delta {delta} where {due} was due")
            }
            None => String::new(),
        };
        let items = ["after the start mark", &late, &stamps];
        Row::new("gap", &format!("{after_ms:.3}"), MS, &self.note(&items))
    }

    fn band_rows(&self) -> [Row; 5] {
        let report = bandwidth(&self.loaded);
        let declared: Vec<String> = (0_u8..)
            .zip(report.histogram)
            .filter(|&(_, units)| units > 0)
            .map(|(limit, units)| format!("{limit}:{units}"))
            .collect();
        let histogram = if declared.is_empty() {
            NONE.to_owned()
        } else {
            declared.join(" ")
        };
        let top = report
            .max()
            .and_then(|limit| band_line(limit).map(|line| (limit, line)));
        let (max, hz) = match top {
            Some((limit, line)) => {
                let spectral = format!("spectral line {line}");
                let max = Row::new(
                    "max_sfb_max",
                    &limit,
                    "band index",
                    &self.note(&[&spectral]),
                );
                let hz = self.rate("max_sfb_hz", HZ, &self.clock, |clock| {
                    let items = [
                        format!("line {line} x clock_hz / {LINES_PER_CLOCK}"),
                        format!("band table made for {TABLE_CLOCK_HZ} Hz"),
                        "a limit the coder declares, not a measured bandwidth".to_owned(),
                        assumed_unit(clock),
                    ];
                    let hz = band_hz(line, clock.hz());
                    (format!("{hz:.3}"), items.join(NOTE_SEPARATOR))
                });
                (max, hz)
            }
            None => (
                Row::new("max_sfb_max", &NONE, "band index", &self.note(&[])),
                Row::new("max_sfb_hz", &NONE, HZ, &self.note(&[])),
            ),
        };
        let over = format!("over {LAST_BAND}");
        [
            Row::new(
                "max_sfb_histogram",
                &histogram,
                "units by band index",
                &self.note(&[]),
            ),
            max,
            Row::new(
                "max_sfb_invalid",
                &report.invalid,
                UNITS,
                &self.note(&[&over]),
            ),
            Row::new(
                "max_sfb_unread",
                &report.unread,
                UNITS,
                &self.note(&["under two bytes"]),
            ),
            hz,
        ]
    }

    /// Every audio SDU as an arrival: when it came and how many units the segment had delivered by then.
    fn arrivals(&self) -> Vec<Arrival> {
        let mut frames_done = 0_u64;
        let arrivals = self.loaded.audio.iter().map(|sdu| {
            // No Rust target has a usize wider than 64 bits, so the fallback is never taken.
            let frames = u64::try_from(sdu.units.len()).unwrap_or(u64::MAX);
            frames_done = frames_done.saturating_add(frames);
            let at_ns = sdu.at_ns;
            Arrival { at_ns, frames_done }
        });
        arrivals.collect()
    }

    fn lead_rows(&self) -> Vec<Row> {
        let own_frame_ns = |clock: &ClockEstimate| NS_PER_SECOND / clock.units_per_second;
        let usable = |frame_ns: f64| frame_ns.is_finite() && frame_ns > 0.0;
        let own = self.clock.as_ref().ok();
        let own = own.filter(|clock| usable(own_frame_ns(clock)));
        let frame_ns = own.map_or(NOMINAL_FRAME_NS, own_frame_ns);
        let Some(found) = lead(&self.arrivals(), self.start_ns, frame_ns) else {
            let window_s = seconds(LEAD_WINDOW_NS);
            let why = format!("fewer than {LEAD_MIN_PACKETS} packets in the first {window_s:.0} s");
            let note = self.note(&[&why]);
            let keys = [
                ("lead_ms", MS),
                ("burst_packets", PACKETS),
                ("burst_frames", "frames"),
                ("burst_ms", MS),
            ];
            let rows = keys.map(|(key, unit)| Row::new(key, &NONE, unit, &note));
            return rows.into();
        };
        // The line and the burst are both cut by the frame length, so every row says where the length came from.
        let frame = if own.is_some() {
            "frame from clock_units_per_second".to_owned()
        } else {
            let nominal_ms = NOMINAL_FRAME_NS / NS_PER_MS;
            format!("nominal {nominal_ms:.3} ms frame assumed, not measured")
        };
        let disagree = own.is_some_and(ClockEstimate::disagree);
        let disagree = if disagree { "disagree" } else { "" };
        let gap = if self.gaps.is_empty() { "" } else { "gap" };
        let line = "start mark minus the earliest arrival line";
        let span = "first to last packet of the burst";
        let said = |what: &str| self.note(&[what, &frame, disagree, gap]);
        vec![
            Row::new("lead_ms", &format!("{:.3}", found.lead_ms), MS, &said(line)),
            Row::new("burst_packets", &found.burst_packets, PACKETS, &said("")),
            Row::new("burst_frames", &found.burst_frames, "frames", &said("")),
            Row::new(
                "burst_ms",
                &format!("{:.3}", found.burst_ms),
                MS,
                &said(span),
            ),
        ]
    }

    /// The audio after the stop mark: the pods send until they take the stop, and none of it is in a rate.
    fn tail_rows(&self) -> [Row; 2] {
        let (sdus_key, last_key) = ("after_stop_audio_sdus", "after_stop_audio_ms");
        let Some(tail) = &self.tail else {
            let note = self.note(&["no stop mark ends this start"]);
            return [
                Row::new(sdus_key, &NONE, PACKETS, &note),
                Row::new(last_key, &NONE, MS, &note),
            ];
        };
        let last_ms = tail
            .last_after_ns
            .map_or_else(|| NONE.to_owned(), |ns| format!("{:.3}", millis(ns)));
        let counted = "from the stop mark to the next start mark; in no rate";
        let timed = "stop mark to the last audio SDU after it";
        [
            Row::new(sdus_key, &tail.sdus, PACKETS, &self.note(&[counted])),
            Row::new(last_key, &last_ms, MS, &self.note(&[timed])),
        ]
    }
}

#[cfg(test)]
mod tests;
