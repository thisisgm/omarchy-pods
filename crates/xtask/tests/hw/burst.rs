//! The burst check C8: the command opens the pods' control channel itself, through the trap, after the test unit stopped.

use std::time::{Duration, Instant};

use pods_fake::Fault;
use pods_samples::{METADATA_SERIAL, host};
use pods_trap::Outcome;
use rustix::io::Errno;

use super::bench::{Bench, DEVICE_NAME, assert_restarted_after_the_run, text};

/// How long the fake pods may take to see a channel closed once the run has ended.
const CLOSE_DEADLINE: Duration = Duration::from_secs(2);
/// How long C8 waits for each handshake acknowledgement, the bound its failure line names.
const ACK_WAIT: Duration = Duration::from_secs(2);
/// The restore after a failed C8 and the command's exit.
const AFTER_ACK_WAIT: Duration = Duration::from_secs(1);
/// The C8 line for `pods_samples::first_burst()` after its two acknowledgements.
const SAMPLE_COUNTS: &str = "36 packets; opcodes 02 x1, 04 x1, 06 x1, 08 x1, 09 x22, 0c x3, 0e x1, 1d x1, 2e x2, 4e x1, 53 x1, 63 x1; settings 01 to 16; unparsed none";

#[test]
fn the_burst_check_passes_against_the_fake_pods() {
    let bench = Bench::new();
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(
        stdout.contains(&format!("C8 burst: passed\n  {SAMPLE_COUNTS}\n")),
        "{stdout}"
    );
    let heard: Vec<Vec<u8>> = bench
        .pods()
        .heard()
        .into_iter()
        .map(|heard| heard.sdu)
        .collect();
    let opening = [
        host::handshake(),
        host::set_specific_features(),
        host::request_notifications(),
    ];
    assert_eq!(heard, opening);
    bench.pods().wait_closed(1, CLOSE_DEADLINE).unwrap();
    let dials = bench.dials();
    assert_eq!(dials.len(), 1, "{dials:?}");
    assert_eq!(dials[0].outcome, Outcome::Accepted);
    assert_eq!(bench.sockets(), 1);
    assert_eq!(
        bench.read("units-at-dial"),
        "librepods-hw.service inactive\nlibrepods.service inactive"
    );
}

#[test]
fn a_withheld_battery_report_fails_the_check_and_names_opcode_04() {
    let bench = Bench::new();
    bench.pods().fault(Fault::WithholdBattery);
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    let without_battery = SAMPLE_COUNTS
        .replace("36 packets", "35 packets")
        .replace("04 x1, ", "");
    let expected = format!(
        "C8 burst: FAILED\n  {without_battery}\n  missing opcodes 04\n  extra opcodes none\n  missing settings none\n  extra settings none\n  unparsed opcodes none\n  packets without an opcode 0\n"
    );
    assert!(stdout.contains(&expected), "{stdout}");
}

#[test]
fn a_battery_report_that_does_not_parse_is_named_unparsed_not_missing() {
    let bench = Bench::new();
    bench.pods().fault(Fault::CutBattery);
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    let counts = SAMPLE_COUNTS.replace("unparsed none", "unparsed 04 x1");
    let expected = format!(
        "C8 burst: FAILED\n  {counts}\n  missing opcodes none\n  extra opcodes none\n  missing settings none\n  extra settings none\n  unparsed opcodes 04\n  packets without an opcode 0\n"
    );
    assert!(stdout.contains(&expected), "{stdout}");
}

#[test]
fn a_packet_cut_inside_its_opcode_fails_the_check() {
    let bench = Bench::new();
    bench.pods().fault(Fault::TrailingFragment);
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    let counts = SAMPLE_COUNTS.replace("36 packets", "37 packets");
    let expected = format!(
        "C8 burst: FAILED\n  {counts}\n  missing opcodes none\n  extra opcodes none\n  missing settings none\n  extra settings none\n  unparsed opcodes none\n  packets without an opcode 1\n"
    );
    assert!(stdout.contains(&expected), "{stdout}");
}

#[test]
fn a_device_bluez_reports_disconnected_is_never_dialled() {
    let bench = Bench::new();
    bench.set("pods-leave-after-idle", "");
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains("C8 burst: FAILED\n  no connected AirPods\n"),
        "{stdout}"
    );
    assert_eq!(bench.dials(), []);
    assert_eq!(bench.sockets(), 0);
}

#[test]
fn pods_that_refuse_the_channel_fail_the_check_with_the_errno() {
    let bench = Bench::new();
    bench.refuse_with(Errno::CONNREFUSED);
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.contains(
            "C8 burst: FAILED\n  could not connect the L2CAP channel: Connection refused (os error 111)\n"
        ),
        "{stdout}"
    );
    let dials = bench.dials();
    assert_eq!(dials.len(), 1, "{dials:?}");
    assert_eq!(dials[0].outcome, Outcome::Refused(Errno::CONNREFUSED));
}

#[test]
fn silent_pods_fail_the_check_within_the_acknowledgement_wait() {
    let bench = Bench::new();
    bench.pods().fault(Fault::NeverAcknowledge);
    let output = bench.run(&[]);
    let ended = Instant::now();
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    let failure = format!(
        "C8 burst: FAILED\n  waited {} s for the handshake acknowledgement\n",
        ACK_WAIT.as_secs()
    );
    assert!(stdout.contains(&failure), "{stdout}");
    let dials = bench.dials();
    assert_eq!(dials.len(), 1, "{dials:?}");
    let waited = ended.duration_since(dials[0].at);
    assert!(waited >= ACK_WAIT, "{waited:?}");
    assert!(waited < ACK_WAIT + AFTER_ACK_WAIT, "{waited:?}");
    assert_restarted_after_the_run(&bench);
}

#[test]
fn the_installed_service_is_restored_when_the_burst_check_fails() {
    let bench = Bench::new();
    bench.pods().fault(Fault::WithholdBattery);
    let output = bench.run(&[]);
    let stdout = text(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("C8 burst: FAILED"), "{stdout}");
    assert!(stdout.ends_with("result: FAILED\n"), "{stdout}");
    assert_restarted_after_the_run(&bench);
    assert!(text(&output.stderr).ends_with("xtask hw: librepods.service is as the run found it\n"));
}

#[test]
fn the_burst_output_holds_no_address_and_no_name() {
    for fault in [Fault::None, Fault::WithholdBattery] {
        let bench = Bench::new();
        bench.pods().fault(fault);
        let output = bench.run(&[]);
        let all = text(&output.stdout) + &text(&output.stderr);
        assert!(all.contains("C8 burst: "), "{all}");
        assert!(
            all.contains("36 packets") || all.contains("35 packets"),
            "{all}"
        );
        for private in [DEVICE_NAME, "02:00:00", "02_00_00", METADATA_SERIAL] {
            assert!(!all.contains(private), "{fault:?} printed {private}: {all}");
        }
    }
}
