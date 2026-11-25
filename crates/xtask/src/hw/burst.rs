//! C8, the burst check: the pods' first burst after the opening exchange, held to the sample the fake pods play.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::LowerHex;
use std::time::{Duration, Instant};

use pods_proto::addr::BdAddr;
use pods_proto::handshake::{Handshake, Step};
use pods_proto::packet::{Incoming, classify, opcode_of};
use pods_proto::packets::{CONTROL_PSM, HANDSHAKE};
use pods_sys::l2cap::{self, ChannelError, Received};
use rustix::io::Errno;

use super::clock::{Clock, SystemClock};
use super::lab::Lab;
use super::report::Check;
use super::system::bluez;
use super::unit::{self, UNDER_TEST};
use super::{Interrupted, stopped};
use crate::probe::{CONNECT_TIMEOUT_MS, HANDSHAKE_STEP_TIMEOUT_MS};

/// The burst was measured to end well inside one second.
pub(super) const BURST_WINDOW: Duration = Duration::from_secs(3);
/// The distinct opcodes of `pods_samples::first_burst()` after its two acknowledgements, ascending.
pub(super) const EXPECTED_OPCODES: &[u16] = &[
    0x02, 0x04, 0x06, 0x08, 0x09, 0x0c, 0x0e, 0x1d, 0x2e, 0x4e, 0x53, 0x63,
];
/// The distinct identifiers of its opcode 0x09 packets, ascending.
pub(super) const EXPECTED_SETTINGS: &[u8] = &[
    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10,
    0x11, 0x12, 0x13, 0x14, 0x15, 0x16,
];
/// A judgment, not a measurement: each acknowledgement of the opening exchange gets the probe capture's bound for a step.
const ACK_WAIT: Duration = Duration::from_millis(HANDSHAKE_STEP_TIMEOUT_MS);
/// Larger than the 247 byte metadata packet, the largest of the burst; a longer SDU is noted by its start.
const RECV_BUFFER_LEN: usize = 1024;

/// What the burst held, never a payload.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Tally {
    opcodes: BTreeMap<u16, usize>,
    unparsed: BTreeMap<u16, usize>,
    settings: BTreeSet<u8>,
    without_opcode: usize,
}

/// How a burst's sets differ from the expected ones.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Difference {
    missing_opcodes: Vec<u16>,
    extra_opcodes: Vec<u16>,
    missing_settings: Vec<u8>,
    extra_settings: Vec<u8>,
    unparsed_opcodes: Vec<u16>,
    without_opcode: usize,
}

enum Ended {
    Interrupted,
    Failed(String),
}

impl From<ChannelError> for Ended {
    fn from(error: ChannelError) -> Self {
        Self::Failed(error.to_string())
    }
}

impl Tally {
    /// Keeps nothing of the SDU but its opcode, whether it parsed and a setting's identifier.
    pub(super) fn note(&mut self, sdu: &[u8]) {
        let Some(opcode) = opcode_of(sdu) else {
            self.without_opcode = self.without_opcode.saturating_add(1);
            return;
        };
        count(&mut self.opcodes, opcode);
        match classify(sdu) {
            Ok(Incoming::Control(command)) => {
                self.settings.insert(command.id.0);
            }
            Ok(_) => {}
            Err(_) => count(&mut self.unparsed, opcode),
        }
    }

    pub(super) fn verdict(&self) -> Result<(), Box<Difference>> {
        let (missing_opcodes, extra_opcodes) = compare(EXPECTED_OPCODES, self.opcodes.keys());
        let (missing_settings, extra_settings) = compare(EXPECTED_SETTINGS, &self.settings);
        let difference = Difference {
            missing_opcodes,
            extra_opcodes,
            missing_settings,
            extra_settings,
            unparsed_opcodes: self.unparsed.keys().copied().collect(),
            without_opcode: self.without_opcode,
        };
        if difference == Difference::default() {
            Ok(())
        } else {
            Err(Box::new(difference))
        }
    }

    fn packets(&self) -> usize {
        self.opcodes
            .values()
            .sum::<usize>()
            .saturating_add(self.without_opcode)
    }

    /// `36 packets; opcodes 04 x1, 09 x22; settings 01 to 16; unparsed none`.
    fn line(&self) -> String {
        format!(
            "{} packets; opcodes {}; settings {}; unparsed {}",
            self.packets(),
            counts(&self.opcodes),
            runs(&self.settings),
            counts(&self.unparsed)
        )
    }
}

impl Difference {
    fn lines(&self) -> [String; 6] {
        [
            format!("missing opcodes {}", hex_list(&self.missing_opcodes)),
            format!("extra opcodes {}", hex_list(&self.extra_opcodes)),
            format!("missing settings {}", hex_list(&self.missing_settings)),
            format!("extra settings {}", hex_list(&self.extra_settings)),
            format!("unparsed opcodes {}", hex_list(&self.unparsed_opcodes)),
            format!("packets without an opcode {}", self.without_opcode),
        ]
    }
}

/// What `expected` has that `seen` lacks, and what `seen` has beyond it, each ascending.
fn compare<'a, T: Ord + Copy + 'a>(
    expected: &[T],
    seen: impl IntoIterator<Item = &'a T>,
) -> (Vec<T>, Vec<T>) {
    let expected: BTreeSet<T> = expected.iter().copied().collect();
    let seen: BTreeSet<T> = seen.into_iter().copied().collect();
    (
        expected.difference(&seen).copied().collect(),
        seen.difference(&expected).copied().collect(),
    )
}

fn count(counts: &mut BTreeMap<u16, usize>, opcode: u16) {
    let count = counts.entry(opcode).or_default();
    *count = count.saturating_add(1);
}

/// `04 x1, 09 x22`.
fn counts(counts: &BTreeMap<u16, usize>) -> String {
    joined(
        counts
            .iter()
            .map(|(opcode, count)| format!("{opcode:02x} x{count}")),
    )
}

fn hex_list<T: LowerHex>(items: &[T]) -> String {
    joined(items.iter().map(|item| format!("{item:02x}")))
}

fn joined(items: impl IntoIterator<Item = String>) -> String {
    let items: Vec<String> = items.into_iter().collect();
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.join(", ")
    }
}

/// `01 to 05, 07` for identifiers 1 to 5 and 7.
fn runs(settings: &BTreeSet<u8>) -> String {
    let mut runs: Vec<(u8, u8)> = Vec::new();
    for &identifier in settings {
        match runs.last_mut() {
            Some((_, last)) if last.checked_add(1) == Some(identifier) => *last = identifier,
            _ => runs.push((identifier, identifier)),
        }
    }
    joined(runs.into_iter().map(|(first, last)| {
        if first == last {
            format!("{first:02x}")
        } else {
            format!("{first:02x} to {last:02x}")
        }
    }))
}

/// C8: stops the test unit, then opens the control channel once and tallies the first burst.
pub(super) fn run(lab: &Lab<'_>) -> Result<Check, Interrupted> {
    let mut check = Check::new("C8 burst");
    // The daemon under test holds a channel while it runs, and whether the pods would take a second one is unknown.
    let address = unit::stop(UNDER_TEST).and_then(|()| connected_address());
    let ended = match address {
        Ok(_) if stopped() => Err(Ended::Interrupted),
        Ok(address) => listen(address, lab.waits.burst),
        Err(error) => Err(Ended::Failed(error)),
    };
    match ended {
        Ok(tally) => {
            check.note(tally.line());
            if let Err(difference) = tally.verdict() {
                for line in difference.lines() {
                    check.fail(line);
                }
            }
        }
        Err(Ended::Failed(error)) => check.fail(error),
        Err(Ended::Interrupted) => return Err(Interrupted),
    }
    Ok(check)
}

/// A device `BlueZ` does not report connected is never dialled.
fn connected_address() -> Result<BdAddr, String> {
    let device = bluez()?.connected().ok_or("no connected AirPods")?;
    device
        .address
        .ok_or_else(|| "BlueZ gave no address for the connected AirPods".to_owned())
}

/// Opens the channel, runs the opening exchange and tallies what arrives within `window` of the notification request; the channel closes on return.
fn listen(address: BdAddr, window: Duration) -> Result<Tally, Ended> {
    let timeout = Duration::from_millis(CONNECT_TIMEOUT_MS);
    let connected = l2cap::connect(address, CONTROL_PSM, timeout);
    if stopped() {
        return Err(Ended::Interrupted);
    }
    let channel = connected?;
    let mut receive = |buffer: &mut [u8], left| {
        channel.set_recv_timeout(left)?;
        channel.recv(buffer)
    };
    let mut buffer = vec![0; RECV_BUFFER_LEN];
    channel.send(&HANDSHAKE)?;
    let mut handshake = Handshake::default();
    let clock = SystemClock;
    let mut asked = clock.now();
    while handshake.step() != Step::Ready {
        let len = next_acknowledgement(&clock, &mut buffer, asked, handshake.step(), &mut receive)?;
        if let Some(request) = handshake.on_packet(buffer.get(..len).unwrap_or_default()) {
            channel.send(request)?;
            asked = clock.now();
        }
    }
    tally_burst(&clock, &mut buffer, asked, window, &mut receive)
}

fn next_acknowledgement(
    clock: &dyn Clock,
    buffer: &mut [u8],
    since: Instant,
    step: Step,
    receive: &mut impl FnMut(&mut [u8], Duration) -> Result<Received, ChannelError>,
) -> Result<usize, Ended> {
    next(clock, buffer, since, ACK_WAIT, receive)?
        .ok_or_else(|| Ended::Failed(format!("waited {} s for {step}", ACK_WAIT.as_secs())))
}

fn tally_burst(
    clock: &dyn Clock,
    buffer: &mut [u8],
    since: Instant,
    window: Duration,
    receive: &mut impl FnMut(&mut [u8], Duration) -> Result<Received, ChannelError>,
) -> Result<Tally, Ended> {
    let mut tally = Tally::default();
    while let Some(len) = next(clock, buffer, since, window, receive)? {
        tally.note(buffer.get(..len).unwrap_or_default());
    }
    Ok(tally)
}

/// The length of the next SDU in `buffer`, or none once `limit` after `since` has passed; a stop signal ends the wait.
fn next(
    clock: &dyn Clock,
    buffer: &mut [u8],
    since: Instant,
    limit: Duration,
    receive: &mut impl FnMut(&mut [u8], Duration) -> Result<Received, ChannelError>,
) -> Result<Option<usize>, Ended> {
    loop {
        // Read on every turn: a signal that lands between two reads interrupts neither.
        if stopped() {
            return Err(Ended::Interrupted);
        }
        let left = limit.saturating_sub(clock.now().saturating_duration_since(since));
        if left.is_zero() {
            return Ok(None);
        }
        match receive(buffer, left) {
            Ok(Received::Sdu(len)) => return Ok(Some(len)),
            Ok(Received::Truncated { .. }) => return Ok(Some(buffer.len())),
            Ok(Received::Closed) => {
                return Err(Ended::Failed("the pods closed the channel".to_owned()));
            }
            Ok(Received::TimedOut) | Err(ChannelError::Recv(Errno::INTR)) => {}
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::{Difference, EXPECTED_OPCODES, EXPECTED_SETTINGS, Tally};

    fn tally(sdus: &[Vec<u8>]) -> Tally {
        let mut tally = Tally::default();
        for sdu in sdus {
            tally.note(sdu);
        }
        tally
    }

    #[test]
    fn the_expected_sets_equal_the_sample_burst() {
        let sample = tally(&pods_samples::first_burst()[2..]);
        let opcodes: Vec<u16> = sample.opcodes.keys().copied().collect();
        let settings: Vec<u8> = sample.settings.iter().copied().collect();
        assert_eq!(
            (opcodes.as_slice(), settings.as_slice()),
            (EXPECTED_OPCODES, EXPECTED_SETTINGS)
        );
        assert_eq!(sample.packets(), 36);
        assert_eq!(sample.verdict(), Ok(()));
    }

    #[test]
    fn a_missing_and_an_extra_opcode_are_both_named() {
        let mut burst = pods_samples::first_burst();
        burst.retain(|sdu| *sdu != pods_samples::battery());
        burst.push(pods_samples::features_ack());
        burst.push(pods_samples::setting(0x40, 1));
        let difference = Difference {
            missing_opcodes: vec![0x04],
            extra_opcodes: vec![0x2b],
            missing_settings: Vec::new(),
            extra_settings: vec![0x40],
            unparsed_opcodes: Vec::new(),
            without_opcode: 0,
        };
        assert_eq!(tally(&burst[2..]).verdict(), Err(Box::new(difference)));
    }

    #[test]
    fn an_sdu_shorter_than_its_header_is_counted_and_adds_no_opcode() {
        let short = tally(&[vec![0x04, 0x00, 0x04, 0x00, 0x09]]);
        assert_eq!((short.packets(), short.opcodes.len()), (1, 0));
        assert!(short.settings.is_empty());
        assert_eq!(
            short
                .verdict()
                .map_err(|difference| difference.without_opcode),
            Err(1)
        );
    }

    proptest! {
        #[test]
        fn any_bytes_can_be_noted(sdu in proptest::collection::vec(any::<u8>(), 0..=512)) {
            let mut tally = Tally::default();
            tally.note(&sdu);
            prop_assert_eq!(tally.packets(), 1);
            prop_assert!(tally.opcodes.values().sum::<usize>() <= 1);
            prop_assert!(tally.unparsed.values().sum::<usize>() <= tally.opcodes.values().sum::<usize>());
        }
    }
}

#[cfg(test)]
mod clock_tests {
    use super::super::Waits;
    use super::super::clock::{Clock, PausedClock};
    use super::{RECV_BUFFER_LEN, Received, tally_burst};
    use std::time::Duration;

    #[test]
    fn the_burst_ends_at_its_deadline_even_after_receiving_an_sdu() {
        let clock = PausedClock::default();
        let mut buffer = [0; RECV_BUFFER_LEN];
        let mut waits = Vec::new();
        let tally = tally_burst(
            &clock,
            &mut buffer,
            clock.now(),
            Waits::default().burst,
            &mut |buffer, left| {
                waits.push(left);
                if waits.len() == 1 {
                    clock.pause(Duration::from_secs(2));
                    let packet = pods_samples::battery();
                    assert!(packet.len() <= buffer.len());
                    buffer[..packet.len()].copy_from_slice(&packet);
                    Ok(Received::Sdu(packet.len()))
                } else {
                    clock.pause(left);
                    Ok(Received::TimedOut)
                }
            },
        )
        .unwrap_or_else(|_| panic!("the burst should end normally"));
        assert_eq!(tally.opcodes.values().sum::<usize>(), 1);
        assert_eq!(waits, [Duration::from_secs(3), Duration::from_secs(1)]);
        assert_eq!(clock.elapsed(), Duration::from_secs(3));
    }
}

#[cfg(test)]
mod acknowledgement_tests {
    use super::super::clock::{Clock, PausedClock};
    use super::{RECV_BUFFER_LEN, Received, Step, next_acknowledgement};
    use std::time::Duration;

    #[test]
    fn an_unanswered_handshake_fails_at_the_acknowledgement_deadline() {
        let clock = PausedClock::default();
        let mut buffer = [0; RECV_BUFFER_LEN];
        let began = clock.now();
        let result = next_acknowledgement(
            &clock,
            &mut buffer,
            began,
            Step::AwaitHandshakeAck,
            &mut |_, left| {
                assert_eq!(left, Duration::from_secs(2));
                clock.pause(left);
                Ok(Received::TimedOut)
            },
        );
        assert!(
            matches!(result, Err(super::Ended::Failed(reason)) if reason == "waited 2 s for the handshake acknowledgement")
        );
        assert_eq!(clock.elapsed(), Duration::from_secs(2));
    }
}
