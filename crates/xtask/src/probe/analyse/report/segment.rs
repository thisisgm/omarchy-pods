//! The rows of one start mark: its trial, its rates, the shape of its stream, its lead and the audio after its stop.

use pods_proto::sdulog::Record;
use pods_proto::uplink::{AUDIO_HEADER_LEN, OPAQUE_LEN};

use super::super::bands::{
    LAST_BAND, LINES_PER_CLOCK, TABLE_CLOCK_HZ, band_hz, band_line, bandwidth,
};
use super::super::census::{Trial, trials};
use super::super::coarse::{
    COARSE_MIN_SDUS, COARSE_MIN_SPAN_NS, COARSE_OPENING_NS, HALVES_WITHIN_PERCENT, NoCoarse,
    coarse_clock,
};
use super::super::input::Mark;
use super::super::lead::{Arrival, LEAD_MIN_PACKETS, LEAD_WINDOW_NS, lead};
use super::super::load::{Loaded, UPLINK, Window, load, segment, tails, windows};
use super::super::rates::{
    ClockEstimate, THIN_MIN_SDUS, THIN_MIN_SPAN_NS, Thin, bitrate, cadence, clock,
};
use super::super::shape::{
    FieldKind, Gap, LATE_ARRIVAL_NS, ModalDelta, StampBreak, StampReport, StampUnit, gaps,
    length_agrees, opaque_fields, stamps,
};
use super::super::stats::{NS_PER_MS, NS_PER_SECOND, millis, seconds, share};
use super::{
    BIT_PER_SECOND, BYTES, CLOCK_KEY, COARSE_CLOCK_KEY, HZ, MS, NONE, NOTE_SEPARATOR, PACKETS,
    REFUSED, Row, UNITS, labelled,
};

/// Samples in one unit, which turns the unit clock into a sample clock; no packet states it, so every row scaled by it says it is assumed.
const SAMPLES_PER_UNIT: u32 = 480;
/// The duration of one unit when the segment is too thin to give its own.
const NOMINAL_FRAME_NS: f64 = 7_500_000.0;
/// The sample clocks the measured one is set against.
const REFERENCE_HZ: [u32; 2] = [64_000, 48_000];
const GAPS_LISTED: usize = 20;
const OPAQUE_AT: usize = AUDIO_HEADER_LEN - OPAQUE_LEN;
const HEADER_END: usize = AUDIO_HEADER_LEN - 1;
/// The opaque bytes checked as a little endian length, each with the bytes of the SDU it leaves out.
const LENGTH_FIELDS: [(usize, usize); 2] = [(0, 10), (6, 16)];
const STAMP_TICKS: &str = "stamp ticks";
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

pub(super) fn start_rows(records: &[Record], marks: &[Mark], whole: &Loaded) -> Vec<Row> {
    let trials = trials(whole, marks);
    let starts = windows(marks, UPLINK).into_iter();
    let starts = starts.zip(tails(marks, UPLINK)).zip(&trials);
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
        rows.push(start.coarse_row());
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

/// What a stretch held against the `min_sdus` over `min_span_ns` a figure needs.
fn thin_note(thin: &Thin, min_sdus: usize, min_span_ns: u64) -> String {
    format!(
        "{} audio SDUs over {:.3} s; needs {min_sdus} over {:.0} s",
        thin.sdus,
        seconds(thin.span_ns),
        seconds(min_span_ns)
    )
}

/// What every figure scaled by the unit length says: the length is not read from the stream.
fn assumed_unit(samples: u32) -> String {
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
    /// A lost SDU bends every rate of its segment, so each of them says the segment has one.
    fn gap_flag(&self) -> &'static str {
        if self.gaps.is_empty() { "" } else { "gap" }
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
                Row::new(
                    key,
                    &value,
                    unit,
                    &labelled(&self.label, &[&derivation, self.gap_flag()]),
                )
            }
            Err(thin) => {
                let held = thin_note(thin, THIN_MIN_SDUS, THIN_MIN_SPAN_NS);
                Row::new(key, &REFUSED, unit, &labelled(&self.label, &[&held]))
            }
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
                &labelled(
                    &self.label,
                    &["start mark to the first control packet from the pods"],
                ),
            ),
            Row::new(
                "trial_audio_ms",
                &ms(trial.audio_after_ns),
                MS,
                &labelled(&self.label, &["start mark to the first audio SDU"]),
            ),
            Row::new(
                "segment_audio_sdus",
                &audio.len(),
                PACKETS,
                &labelled(&self.label, &[]),
            ),
            Row::new(
                "segment_span_s",
                &format!("{:.3}", seconds(span_ns)),
                "s",
                &labelled(&self.label, &["first to last audio SDU"]),
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
            let assumed = assumed_unit(clock.samples_per_unit);
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
            self.rate(CLOCK_KEY, HZ, clock, |clock| {
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

    /// The clock a start too short for the slope still gives, or why it gives none.
    fn coarse_row(&self) -> Row {
        let opening = format!(
            "counted from {:.0} s after the first audio SDU",
            seconds(COARSE_OPENING_NS)
        );
        let allowed = format!("of {HALVES_WITHIN_PERCENT:.0} allowed");
        let (value, items) = match coarse_clock(&Arrival::of(&self.loaded), SAMPLES_PER_UNIT) {
            Ok(coarse) => {
                let (sdus, span_s) = (coarse.sdus, seconds(coarse.span_ns));
                let (samples, apart) = (coarse.samples_per_unit, coarse.halves_apart_percent);
                let items = vec![
                    format!("units over a span of {span_s:.3} s x {samples} samples per unit"),
                    format!("{sdus} audio SDUs, {opening}"),
                    format!("halves {apart:.2} percent apart, {allowed}"),
                    "the pace of the arrivals, which is the clock only once the backlog is delivered"
                        .to_owned(),
                    assumed_unit(samples),
                    self.gap_flag().to_owned(),
                ];
                (format!("{:.0}", coarse.hz()), items)
            }
            Err(NoCoarse::NoAudio) => (
                NONE.to_owned(),
                vec!["no audio SDU with a whole unit".to_owned()],
            ),
            Err(NoCoarse::Thin(thin)) => (
                REFUSED.to_owned(),
                vec![
                    thin_note(&thin, COARSE_MIN_SDUS, COARSE_MIN_SPAN_NS),
                    opening,
                ],
            ),
            Err(NoCoarse::Unsteady {
                halves_apart_percent,
            }) => {
                let apart = halves_apart_percent.map_or_else(
                    || "a half of the span has no time in it".to_owned(),
                    |apart| format!("halves of the span {apart:.2} percent apart, {allowed}"),
                );
                let why = format!("{apart}: the delay of the arrivals changes inside it");
                (REFUSED.to_owned(), vec![why, opening])
            }
        };
        let items: Vec<&str> = items.iter().map(String::as_str).collect();
        Row::new(COARSE_CLOCK_KEY, &value, HZ, &labelled(&self.label, &items))
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
                Row::new(
                    key,
                    &modal.delta,
                    STAMP_TICKS,
                    &labelled(&self.label, &[&derivation]),
                )
            }
            None => Row::new(key, &NONE, STAMP_TICKS, &labelled(&self.label, &[])),
        };
        let unit = match (self.stamps.implied(), self.stamps.per_unit) {
            (Some(unit), Some(modal)) => {
                let name = match unit {
                    StampUnit::Samples => "samples",
                    StampUnit::Microseconds => "microseconds",
                    StampUnit::Unknown(_) => "unknown",
                };
                let derivation = format!("modal delta per unit {}", modal.delta);
                Row::new(
                    "stamp_unit",
                    &name,
                    "name",
                    &labelled(&self.label, &[&derivation]),
                )
            }
            _ => Row::new("stamp_unit", &NONE, "name", &labelled(&self.label, &[])),
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
            &labelled(&self.label, &[&order, &format!("{sdus} SDUs"), seen]),
        )
    }

    fn length_rows(&self) -> [Row; 2] {
        let sdus = self.loaded.audio.len();
        LENGTH_FIELDS.map(|(at, less)| {
            let header_byte = OPAQUE_AT.saturating_add(at);
            let agree = length_agrees(&self.loaded, at, less);
            let rule = format!(
                "of {sdus} SDUs; little endian u16 from header byte {header_byte} against the SDU length less {less}"
            );
            let key = format!("header_length_at_{header_byte}");
            Row::new(&key, &agree, PACKETS, &labelled(&self.label, &[&rule]))
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
        let count = Row::new(
            "gaps",
            &self.gaps.len(),
            "gaps",
            &labelled(&self.label, &[&rule, &cut]),
        );
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
        Row::new(
            "gap",
            &format!("{after_ms:.3}"),
            MS,
            &labelled(&self.label, &items),
        )
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
                    &labelled(&self.label, &[&spectral]),
                );
                let hz = self.rate("max_sfb_hz", HZ, &self.clock, |clock| {
                    let items = [
                        format!("line {line} x clock_hz / {LINES_PER_CLOCK}"),
                        format!("band table made for {TABLE_CLOCK_HZ} Hz"),
                        "a limit the coder declares, not a measured bandwidth".to_owned(),
                        assumed_unit(clock.samples_per_unit),
                    ];
                    let hz = band_hz(line, clock.hz());
                    (format!("{hz:.3}"), items.join(NOTE_SEPARATOR))
                });
                (max, hz)
            }
            None => (
                Row::new(
                    "max_sfb_max",
                    &NONE,
                    "band index",
                    &labelled(&self.label, &[]),
                ),
                Row::new("max_sfb_hz", &NONE, HZ, &labelled(&self.label, &[])),
            ),
        };
        let over = format!("over {LAST_BAND}");
        [
            Row::new(
                "max_sfb_histogram",
                &histogram,
                "units by band index",
                &labelled(&self.label, &[]),
            ),
            max,
            Row::new(
                "max_sfb_invalid",
                &report.invalid,
                UNITS,
                &labelled(&self.label, &[&over]),
            ),
            Row::new(
                "max_sfb_unread",
                &report.unread,
                UNITS,
                &labelled(&self.label, &["under two bytes"]),
            ),
            hz,
        ]
    }

    fn lead_rows(&self) -> Vec<Row> {
        let own_frame_ns = |clock: &ClockEstimate| NS_PER_SECOND / clock.units_per_second;
        let usable = |frame_ns: f64| frame_ns.is_finite() && frame_ns > 0.0;
        let own = self.clock.as_ref().ok();
        let own = own.filter(|clock| usable(own_frame_ns(clock)));
        let frame_ns = own.map_or(NOMINAL_FRAME_NS, own_frame_ns);
        let Some(found) = lead(&Arrival::of(&self.loaded), self.start_ns, frame_ns) else {
            let window_s = seconds(LEAD_WINDOW_NS);
            let why = format!(
                "fewer than {LEAD_MIN_PACKETS} packets with a whole unit in the first {window_s:.0} s"
            );
            let note = labelled(&self.label, &[&why]);
            let keys = [
                ("lead_ms", MS),
                ("burst_packets", PACKETS),
                ("burst_frames", "frames"),
                ("burst_ms", MS),
            ];
            let rows = keys.map(|(key, unit)| Row::new(key, &REFUSED, unit, &note));
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
        let line = "start mark minus the earliest arrival line";
        let span = "first to last packet of the burst";
        let said = |what: &str| labelled(&self.label, &[what, &frame, disagree, self.gap_flag()]);
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
            let note = labelled(&self.label, &["no stop mark ends this start"]);
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
            Row::new(
                sdus_key,
                &tail.sdus,
                PACKETS,
                &labelled(&self.label, &[counted]),
            ),
            Row::new(last_key, &last_ms, MS, &labelled(&self.label, &[timed])),
        ]
    }
}

#[cfg(test)]
mod tests;
