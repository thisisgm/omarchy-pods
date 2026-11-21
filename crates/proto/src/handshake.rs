//! The three requests that open the control channel, each sent once the pods acknowledge the one before.

use std::fmt;

use crate::packets::{
    FEATURES_ACK_PREFIX, HANDSHAKE, HANDSHAKE_ACK_PREFIX, REQUEST_NOTIFICATIONS,
    SET_SPECIFIC_FEATURES,
};

/// What the handshake waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Step {
    #[default]
    AwaitHandshakeAck,
    AwaitFeaturesAck,
    Ready,
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::AwaitHandshakeAck => "the handshake acknowledgement",
            Self::AwaitFeaturesAck => "the features acknowledgement",
            Self::Ready => "nothing more",
        })
    }
}

/// The opening exchange, in the order parseData in daemon/main.cpp runs it.
#[derive(Debug, Default)]
pub struct Handshake {
    step: Step,
}

impl Handshake {
    /// The request the host sends first.
    #[must_use]
    pub fn first() -> &'static [u8] {
        &HANDSHAKE
    }

    /// Takes one packet from the pods; the reply is the next request, and only the awaited acknowledgement gets one.
    // Sample input: 01 00 04 00 acknowledges the handshake, 04 00 04 00 2b 00 acknowledges the features.
    pub fn on_packet(&mut self, sdu: &[u8]) -> Option<&'static [u8]> {
        match self.step {
            Step::AwaitHandshakeAck if sdu.starts_with(&HANDSHAKE_ACK_PREFIX) => {
                self.step = Step::AwaitFeaturesAck;
                Some(&SET_SPECIFIC_FEATURES)
            }
            Step::AwaitFeaturesAck if sdu.starts_with(&FEATURES_ACK_PREFIX) => {
                self.step = Step::Ready;
                Some(&REQUEST_NOTIFICATIONS)
            }
            Step::AwaitHandshakeAck | Step::AwaitFeaturesAck | Step::Ready => None,
        }
    }

    #[must_use]
    pub fn step(&self) -> Step {
        self.step
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// An ear detection packet, which is ordinary traffic during a handshake.
    const EAR_PACKET: [u8; 8] = [0x04, 0x00, 0x04, 0x00, 0x06, 0x00, 0x00, 0x00];
    /// The longest packet the property test feeds, one receive buffer.
    const LONGEST_PACKET: usize = 4096;

    /// An acknowledgement as the pods send it: the prefix, then bytes the handshake does not read.
    fn with_tail(prefix: &[u8]) -> Vec<u8> {
        [prefix, &[0x00, 0x11, 0x22]].concat()
    }

    #[test]
    fn follows_the_acks_in_order() {
        let mut handshake = Handshake::default();
        assert_eq!(Handshake::first(), HANDSHAKE);
        assert_eq!(handshake.step(), Step::AwaitHandshakeAck);

        let reply = handshake.on_packet(&with_tail(&HANDSHAKE_ACK_PREFIX));
        assert_eq!(reply, Some(SET_SPECIFIC_FEATURES.as_slice()));
        assert_eq!(handshake.step(), Step::AwaitFeaturesAck);

        let reply = handshake.on_packet(&with_tail(&FEATURES_ACK_PREFIX));
        assert_eq!(reply, Some(REQUEST_NOTIFICATIONS.as_slice()));
        assert_eq!(handshake.step(), Step::Ready);

        assert_eq!(handshake.on_packet(&FEATURES_ACK_PREFIX), None);
        assert_eq!(handshake.on_packet(&HANDSHAKE_ACK_PREFIX), None);
        assert_eq!(handshake.step(), Step::Ready);
    }

    #[test]
    fn ignores_other_packets_while_waiting() {
        let mut handshake = Handshake::default();
        let cut_ack = &HANDSHAKE_ACK_PREFIX[..HANDSHAKE_ACK_PREFIX.len() - 1];
        for other in [&EAR_PACKET[..], &[], &[0x01], cut_ack, &HANDSHAKE] {
            assert_eq!(handshake.on_packet(other), None, "{other:02x?}");
            assert_eq!(handshake.step(), Step::AwaitHandshakeAck);
        }

        assert!(handshake.on_packet(&HANDSHAKE_ACK_PREFIX).is_some());
        let cut_ack = &FEATURES_ACK_PREFIX[..FEATURES_ACK_PREFIX.len() - 1];
        for other in [&EAR_PACKET[..], &[], cut_ack, &HANDSHAKE_ACK_PREFIX] {
            assert_eq!(handshake.on_packet(other), None, "{other:02x?}");
            assert_eq!(handshake.step(), Step::AwaitFeaturesAck);
        }
    }

    #[test]
    fn a_features_ack_before_the_handshake_ack_does_not_advance() {
        let mut handshake = Handshake::default();
        assert_eq!(handshake.on_packet(&with_tail(&FEATURES_ACK_PREFIX)), None);
        assert_eq!(handshake.step(), Step::AwaitHandshakeAck);

        let reply = handshake.on_packet(&HANDSHAKE_ACK_PREFIX);
        assert_eq!(reply, Some(SET_SPECIFIC_FEATURES.as_slice()));
        assert_eq!(handshake.step(), Step::AwaitFeaturesAck);
    }

    proptest! {
        #[test]
        fn any_packets_only_ever_get_the_two_requests_in_order(
            packets in proptest::collection::vec(
                proptest::collection::vec(any::<u8>(), 0..=LONGEST_PACKET),
                0..8,
            ),
            acks_at in proptest::collection::vec(0_usize..8, 0..4),
        ) {
            let mut handshake = Handshake::default();
            let mut replies = Vec::new();
            for (at, noise) in packets.iter().enumerate() {
                let ack = match handshake.step() {
                    Step::AwaitHandshakeAck => &HANDSHAKE_ACK_PREFIX[..],
                    Step::AwaitFeaturesAck | Step::Ready => &FEATURES_ACK_PREFIX[..],
                };
                let packet = if acks_at.contains(&at) { ack } else { noise.as_slice() };
                replies.extend(handshake.on_packet(packet));
            }
            let in_order = [&SET_SPECIFIC_FEATURES[..], &REQUEST_NOTIFICATIONS[..]];
            prop_assert!(replies.len() <= in_order.len());
            prop_assert_eq!(&replies[..], &in_order[..replies.len()]);
            let step = [Step::AwaitHandshakeAck, Step::AwaitFeaturesAck, Step::Ready][replies.len()];
            prop_assert_eq!(handshake.step(), step);
        }
    }
}
