//! When the daemon logged a line, by the wall clock the trace is stamped with.

use serde_json::Value;

use super::Window;

/// The journal's stamp on the clock a trace and the trials log share; its monotonic stamp counts from the boot.
const WALL_CLOCK: &str = "__REALTIME_TIMESTAMP";
const MESSAGE: &str = "MESSAGE";

/// When the journal first holds the line `text` inside `window`, in microseconds since 1970.
// Sample input: `{"__REALTIME_TIMESTAMP":"1732480001350000","__MONOTONIC_TIMESTAMP":"812345678","MESSAGE":"link: state populated"}`, one line of `journalctl -o json`.
pub(crate) fn line_us(journal: &str, text: &str, window: &Window) -> Option<u64> {
    let at_us = |line: &str| {
        let Value::Object(entry) = serde_json::from_str(line).ok()? else {
            return None;
        };
        let field = |key: &str| entry.get(key).and_then(Value::as_str);
        let at_us: u64 = field(WALL_CLOCK)?.parse().ok()?;
        (field(MESSAGE)? == text && window.holds(at_us)).then_some(at_us)
    };
    journal.lines().filter_map(at_us).min()
}
