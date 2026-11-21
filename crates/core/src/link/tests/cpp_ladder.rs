//! The ladder values read against the C++ daemon they were copied from.

use cpp_source::{CONTROL_RECONNECT, MAIN};

use crate::link::ladder::{ABSENT_GRACE_MS, FIRST_RETRY_MS, HELD_DELAYS_MS, NOTIFY_REREQUEST_MS};

/// The statements of the C++ `ControlReconnect::delayMs`, so a changed formula fails here before its constants are reused.
const CPP_DELAY: [&str; 3] = [
    "const int boundedAttempt = std::clamp(attempt, 1, maxDoublings);",
    "const int boundedJitter = std::clamp(jitterMs, 0, maxJitterMs);",
    "return baseDelayMs * (1 << (boundedAttempt - 1)) + boundedJitter;",
];

/// The value of the `ControlReconnect` constant `name`; sample input: `inline constexpr int baseDelayMs = 1000;`.
fn reconnect_constant(name: &str) -> u64 {
    let text = CONTROL_RECONNECT.find(None, &format!("inline constexpr int {name} = "), ';');
    text.parse().unwrap_or_else(|_| {
        panic!("daemon/controlreconnect.hpp: {name} is not a whole number: {text}")
    })
}

/// The C++ `delayMs(attempt, 0)`, worked out from the header's own constants.
fn cpp_delay_ms(attempt: u64) -> u64 {
    let bounded = attempt.clamp(1, reconnect_constant("maxDoublings"));
    let doublings = u32::try_from(bounded.saturating_sub(1)).unwrap();
    reconnect_constant("baseDelayMs")
        .checked_shl(doublings)
        .unwrap()
}

#[test]
fn delay_ms_is_the_formula_the_constants_feed() {
    let statements: Vec<&str> = CONTROL_RECONNECT
        .body("inline int delayMs(")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    assert_eq!(statements, CPP_DELAY);
}

#[test]
fn held_delays_are_the_cpp_delays_without_jitter() {
    // Jitter is the only part of the C++ delay not compared; the Rust ladder takes it as zero.
    let limit = reconnect_constant("connectedAttemptLimit");
    let cpp: Vec<u64> = (1..=limit).map(cpp_delay_ms).collect();
    assert_eq!(HELD_DELAYS_MS.as_slice(), cpp);
}

#[test]
fn first_retry_is_the_cpp_first_delay() {
    assert_eq!(FIRST_RETRY_MS, reconnect_constant("firstDelayMs"));
}

/// Reads the C++ default absent probe count; sample input: `int m_retryAttempts = 3;`.
#[test]
fn absent_grace_is_the_first_delay_then_the_absent_probes() {
    // corner: the C++ default of `m_retryAttempts`; `setRetryAttempts` can change it at run time and the Rust grace does not follow.
    let text = MAIN.find(None, "int m_retryAttempts = ", ';');
    let probes: u64 = text.parse().unwrap_or_else(|_| {
        panic!("daemon/main.cpp: m_retryAttempts is not a whole number: {text}")
    });
    let probe_delays: u64 = (1..=probes).map(cpp_delay_ms).sum();
    assert_eq!(
        ABSENT_GRACE_MS,
        reconnect_constant("firstDelayMs") + probe_delays
    );
}

/// Reads the timer inside the `FEATURES_ACK` branch of `parseData`; sample input: `FEATURES_ACK)) { writePacketToSocket(...); QTimer::singleShot(2000, this, [this]() { ... }); } else if`.
#[test]
fn notification_rerequest_is_the_timer_in_parse_data() {
    let parse = MAIN.body("void parseData(");
    let (_, after) = parse
        .split_once("FEATURES_ACK")
        .expect("daemon/main.cpp: parseData has no FEATURES_ACK branch");
    let (branch, _) = after
        .split_once("else if")
        .expect("daemon/main.cpp: no branch follows FEATURES_ACK in parseData");
    let (_, timer) = branch.split_once("QTimer::singleShot(").unwrap_or_else(|| {
        panic!("daemon/main.cpp: no timer in the FEATURES_ACK branch: {branch}")
    });
    let (delay, _) = timer
        .split_once(',')
        .unwrap_or_else(|| panic!("daemon/main.cpp: the FEATURES_ACK timer has no delay: {timer}"));
    assert_eq!(delay.trim(), NOTIFY_REREQUEST_MS.to_string());
}
