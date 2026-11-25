//! When the audio server first listed the sink of a Bluetooth device.

use pods_core::policy::audio::sink_prefix;
use pods_proto::addr::BdAddr;

use super::{Window, stamp_us};

// Sample input: `1732480001.250000000 57\tbluez_output.<device>.1\tPipeWire\ts16le 2ch 48000Hz\tSUSPENDED`, from `date +%s.%N` and `pactl list short sinks`.
fn named_sink<'a>(line: &'a str, prefix: &str, window: &Window) -> Option<(u64, &'a str)> {
    let (stamp, listed) = line.split_once(' ')?;
    let at_us = stamp_us(stamp)?;
    let name = listed.split('\t').nth(1)?;
    let suffix = name.strip_prefix(prefix)?;
    let matches = suffix.is_empty() || suffix.starts_with('.');
    (matches && window.holds(at_us)).then_some((at_us, listed))
}

/// When the log first lists the named device's Bluetooth sink inside `window`, in microseconds since 1970.
pub(crate) fn first_bluez_sink_us(sinks_log: &str, device: BdAddr, window: &Window) -> Option<u64> {
    let prefix = sink_prefix(device);
    sinks_log
        .lines()
        .filter_map(|line| named_sink(line, &prefix, window).map(|(at_us, _)| at_us))
        .min()
}

/// The named sink's first state in the window and each change the snapshots show after it.
pub(super) fn state_changes(
    sinks_log: &str,
    device: BdAddr,
    window: &Window,
) -> Vec<(u64, &'static str)> {
    let prefix = sink_prefix(device);
    let samples = sinks_log.lines().filter_map(|line| {
        let (at_us, listed) = named_sink(line, &prefix, window)?;
        let (_, state) = listed.rsplit_once('\t')?;
        let name = match state {
            "RUNNING" => "running",
            "IDLE" => "idle",
            "SUSPENDED" => "suspended",
            _ => return None,
        };
        Some((at_us, name))
    });
    let mut changes = Vec::new();
    for sample in samples {
        if changes.last().is_none_or(|&(_, state)| state != sample.1) {
            changes.push(sample);
        }
    }
    changes
}
