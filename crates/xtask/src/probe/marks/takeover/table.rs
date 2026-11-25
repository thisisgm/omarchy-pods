//! The takeover command's redacted TSV, one row per trial or one for an SDU log alone.

use std::fmt::Write;

use pods_proto::addr::BdAddr;
use pods_proto::sdulog::Direction;

use super::{Order, PeerClass, SourceRow, TakeoverArgs, connected_devices, sources, stream};
use crate::probe::analyse::input::{LOG_FILE, META_FILE, parse_lines, parse_meta};
use crate::probe::hci::{Cut, MAX_TRACE_BYTES};
use crate::probe::marks::connect::Began;
use crate::probe::marks::journal::line_us;
use crate::probe::marks::sinks::state_changes;
use crate::probe::marks::{
    INCOMPLETE, MarksError, NANOS_PER_MICRO, REASONLESS, STATE_POPULATED_LINE, Window, text,
    trace_extent, up_line, windows,
};
use crate::probe::store;

const HEADER: &str = "trial\tlink\tsources\tconnected_devices\tsuspend_us\tsuspend_from\tclose_us\tclose_from\trestart_us\tended\tup_us\tpopulated_us\tsink_states\tsuspend_after_source_us\tclose_after_source_us\trestart_after_handback_us";
const STREAM_COLUMNS: usize = 12;

type DeviceReport = (u64, usize, Option<u8>);

#[derive(Default)]
struct Reports {
    sources: Vec<SourceRow>,
    devices: Vec<DeviceReport>,
}

impl Reports {
    fn read(args: &TakeoverArgs) -> Result<Self, MarksError> {
        let (Some(sdus), Some(host_file)) = (&args.sdus, &args.host_file) else {
            return Ok(Self::default());
        };
        let host = store::read_device_file(host_file)?;
        let zero_ns = if args.trace.is_some() {
            let meta = text(&sdus.with_file_name(META_FILE))?;
            parse_meta(&meta)?
                .realtime_at_zero_ns
                .ok_or(MarksError::NoSduClock)?
        } else {
            0
        };
        let records = parse_lines(LOG_FILE, &text(sdus)?)?;
        let mut incoming = Vec::new();
        for record in &records {
            if record.direction == Direction::FromPods {
                let at_ns = zero_ns
                    .checked_add(record.at_ns)
                    .ok_or(MarksError::SduClockOverflow)?;
                incoming.push((at_ns / NANOS_PER_MICRO, record.sdu.as_slice()));
            }
        }
        Ok(Self {
            sources: sources(&incoming, host),
            devices: connected_devices(&incoming),
        })
    }
}

fn within<'a, T>(rows: &'a [T], window: &Window, stamp: impl Fn(&T) -> u64) -> &'a [T] {
    let from = rows.partition_point(|row| stamp(row) < window.from_us);
    let to = rows.partition_point(|row| stamp(row) < window.to_us);
    rows.get(from..to).unwrap_or_default()
}

fn time(value: Option<u64>) -> String {
    value.map_or_default(|value| value.to_string())
}

fn delay(value: Option<u64>, origin: Option<u64>) -> String {
    value.zip(origin).map_or_default(|(at_us, origin_us)| {
        i128::from(at_us)
            .saturating_sub(i128::from(origin_us))
            .to_string()
    })
}

fn report_fields(sources: &[SourceRow], devices: &[DeviceReport]) -> [String; 2] {
    let sources = sources.iter().map(|source| {
        let class = match source.class {
            PeerClass::Host => "host",
            PeerClass::Other => "other",
            PeerClass::Zero => "zero",
        };
        let order = match source.order {
            Order::Wire => "wire",
            Order::Reversed => "reversed",
            Order::Neither => "neither",
        };
        format!("{}:{class}:{order}:{:#04x}", source.at_us, source.kind)
    });
    let devices = devices.iter().map(|(at_us, len, count)| {
        let count = count.map_or_default(|count| count.to_string());
        format!("{at_us}:{len}:{count}")
    });
    [
        sources.collect::<Vec<_>>().join(","),
        devices.collect::<Vec<_>>().join(","),
    ]
}

fn signal_fields(signal: Option<(u64, Began)>) -> [String; 2] {
    let side = match signal {
        Some((_, Began::Host)) => "host",
        Some((_, Began::Pods)) => "pods",
        None => "",
    };
    [time(signal.map(|(at_us, _)| at_us)), side.to_owned()]
}

fn append_row(
    table: &mut String,
    trial: Option<u32>,
    link: &str,
    reports: [String; 2],
    stream: [String; STREAM_COLUMNS],
) {
    let trial = trial.map_or_default(|number| number.to_string());
    let row = [trial, link.to_owned()]
        .into_iter()
        .chain(reports)
        .chain(stream)
        .collect::<Vec<_>>()
        .join("\t");
    let _written = writeln!(table, "{row}");
}

struct Capture {
    trace: Vec<u8>,
    device: BdAddr,
    up: String,
    journal: String,
    sinks: String,
}

impl Capture {
    fn append_trial(
        &self,
        table: &mut String,
        reports: &Reports,
        window: &Window,
        incomplete: bool,
    ) -> Result<(), MarksError> {
        let sources = within(&reports.sources, window, |source| source.at_us);
        let devices = within(&reports.devices, window, |&(at_us, _, _)| at_us);
        let report_fields = report_fields(sources, devices);
        let mut source_order = sources.iter();
        let first_other = source_order.find(|row| row.class == PeerClass::Other);
        let handback_us = source_order
            .find(|row| row.class != PeerClass::Other)
            .map(|row| row.at_us);
        let trial = Some(window.trial);
        let sink_states = state_changes(&self.sinks, self.device, window)
            .into_iter()
            .map(|(at_us, state)| format!("{at_us}:{state}"))
            .collect::<Vec<_>>()
            .join(",");
        let marks = stream(&self.trace, self.device, window, handback_us)?;
        let (suspend, close, restart) = marks
            .as_ref()
            .map_or_default(|marks| (marks.suspend_us, marks.close_us, marks.restart_us));
        let link = match (marks.is_some(), incomplete) {
            (true, _) => "present",
            (false, true) => INCOMPLETE,
            (false, false) => "missing",
        };
        let [suspend_us, suspend_from] = signal_fields(suspend);
        let [close_us, close_from] = signal_fields(close);
        let ended = match marks.as_ref().and_then(|marks| marks.ended) {
            Some(end) => end
                .reason
                .map_or_else(|| REASONLESS.to_owned(), |reason| format!("{reason:#04x}")),
            None if incomplete && marks.is_some() => INCOMPLETE.to_owned(),
            None => String::new(),
        };
        let journal_mark = |line| {
            marks
                .as_ref()
                .and_then(|marks| line_us(&self.journal, line, &marks.lifetime))
        };
        let other_us = first_other.map(|row| row.at_us);
        let fields = [
            suspend_us,
            suspend_from,
            close_us,
            close_from,
            time(restart),
            ended,
            time(journal_mark(&self.up)),
            time(journal_mark(STATE_POPULATED_LINE)),
            sink_states,
            delay(suspend.map(|(at_us, _)| at_us), other_us),
            delay(close.map(|(at_us, _)| at_us), other_us),
            delay(restart, handback_us),
        ];
        append_row(table, trial, link, report_fields, fields);
        Ok(())
    }
}

pub(in crate::probe::marks) fn table(
    args: &TakeoverArgs,
) -> Result<(String, Option<Cut>), MarksError> {
    let reports = Reports::read(args)?;
    let mut table = format!("{HEADER}\n");
    let (Some(trace), Some(device), Some(trials)) = (&args.trace, &args.device_file, &args.trials)
    else {
        let reports = report_fields(&reports.sources, &reports.devices);
        append_row(&mut table, None, "", reports, Default::default());
        return Ok((table, None));
    };
    let capture = Capture {
        trace: store::read_trace(trace, MAX_TRACE_BYTES)?,
        device: store::read_device_file(device)?,
        up: up_line(),
        journal: args
            .journal
            .as_deref()
            .map(text)
            .transpose()?
            .unwrap_or_default(),
        sinks: args
            .sinks
            .as_deref()
            .map(text)
            .transpose()?
            .unwrap_or_default(),
    };
    let (cut, last_record_us) = trace_extent(&capture.trace)?;
    for window in windows(&text(trials)?)? {
        let incomplete = cut.is_some() && last_record_us.is_none_or(|last| last < window.to_us);
        capture.append_trial(&mut table, &reports, &window, incomplete)?;
    }
    Ok((table, cut))
}
