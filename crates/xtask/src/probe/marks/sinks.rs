//! When the audio server first listed the sink of a Bluetooth device.

use pods_core::policy::audio::sink_prefix;
use pods_proto::addr::BdAddr;

use super::{Window, stamp_us};

/// When the log first lists the named device's Bluetooth sink inside `window`, in microseconds since 1970.
// Sample input: `1732480001.250000000 57\tbluez_output.02_00_00_00_00_01.1\tPipeWire\ts16le 2ch 48000Hz\tSUSPENDED`, the output of `date +%s.%N`, a space, then one line of `pactl list short sinks`.
pub(crate) fn first_bluez_sink_us(sinks_log: &str, device: BdAddr, window: &Window) -> Option<u64> {
    let prefix = sink_prefix(device);
    let at_us = |line: &str| {
        let (stamp, listed) = line.split_once(' ')?;
        let at_us = stamp_us(stamp)?;
        let name = listed.split('\t').nth(1)?;
        let suffix = name.strip_prefix(&prefix)?;
        let named_sink = suffix.is_empty() || suffix.starts_with('.');
        (named_sink && window.holds(at_us)).then_some(at_us)
    };
    sinks_log.lines().filter_map(at_us).min()
}
