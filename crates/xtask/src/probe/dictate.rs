//! The walk of the pods' attribute table, and what it says about the four handles the dictation stream uses.

use std::fmt;

use pods_proto::att::{self, AttPdu, Uuid};
use pods_proto::doap::{self, ARM_MAX_REQUESTS, Handles};

/// How long the walk waits for each answer; chosen here to match one step of the handshake.
pub(crate) const HANDLE_WALK_TIMEOUT_MS: u32 = 2000;
/// The lowest attribute handle (Bluetooth Core Specification 6.1, Vol 3, Part F, 3.2.2).
const FIRST_HANDLE: u16 = 0x0001;
/// The highest attribute handle, where every request of the walk ends (Vol 3, Part F, 3.2.2).
const LAST_HANDLE: u16 = 0xffff;
/// The table of the one firmware seen, which the walk checks before the run writes to it.
const TABLE: Handles = Handles::OBSERVED;
/// The handles the dictation stream is written to and notified on, in rising order.
const NEEDED: [u16; 4] = [TABLE.start, TABLE.stop, TABLE.send_data, TABLE.event];
/// The write that asks the pods to start the stream.
pub(crate) const START: [u8; 5] = doap::start(&TABLE);
/// The write that asks the pods to stop it.
pub(crate) const STOP: [u8; 6] = doap::stop(&TABLE, doap::STOP_REASON_HOST);

/// The one request of `att-map`: `08 01 00 ff ff 03 28`, every characteristic declaration of the table (Vol 3, Part G, 4.6.1).
pub(crate) fn declarations_request() -> [u8; 7] {
    att::read_by_type_request(FIRST_HANDLE, LAST_HANDLE, att::CHARACTERISTIC_DECLARATION)
}

/// An attribute type as a table of handles shows it: `0x2803`, or the 32 hex digits of a 128 bit one, most significant octet first as a UUID is written; the wire holds that octet last.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TypeText<'a>(pub(crate) Uuid<'a>);

impl fmt::Display for TypeText<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Uuid::Short(short) => write!(f, "{short:#06x}"),
            Uuid::Long(long) => long
                .iter()
                .rev()
                .try_for_each(|byte| write!(f, "{byte:02x}")),
        }
    }
}

/// What the pods' own listing says about the four handles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HandleCheck {
    /// All four are listed.
    Present,
    /// The pods listed attributes, and these of the four were not among them.
    Missing(Vec<u16>),
    /// The pods listed no attribute at all: no answer, an error, or a reply the walk could not follow.
    NoListing,
}

impl fmt::Display for HandleCheck {
    // Sample output: `missing 0x000a 0x0010`, the handles in rising order.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Present => f.write_str("present"),
            Self::Missing(handles) => {
                f.write_str("missing")?;
                handles
                    .iter()
                    .try_for_each(|handle| write!(f, " {handle:#06x}"))
            }
            Self::NoListing => f.write_str("no listing"),
        }
    }
}

/// Where a walk stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WalkState {
    /// A request is out.
    Asking,
    /// The pods ended their table: an error response, or a listing that reached the highest handle.
    Ended,
    /// `ARM_MAX_REQUESTS` were answered and the table had not ended.
    Cut,
    /// The pods sent a reply the walk cannot follow, so what is out on the channel is not known.
    Lost,
}

/// Reads the whole attribute table with Find Information requests, each from the handle after the last one listed (Vol 3, Part G, 4.7.1).
#[derive(Debug)]
pub(crate) struct HandleWalk {
    /// The first handle of the request that is out.
    start: u16,
    /// How many requests have gone out, the first among them.
    requests: u32,
    /// Which of `NEEDED` the pods have listed, in its order.
    seen: [bool; 4],
    /// Whether the pods have listed any attribute.
    listed: bool,
    state: WalkState,
}

impl Default for HandleWalk {
    fn default() -> Self {
        Self {
            start: FIRST_HANDLE,
            requests: 1,
            seen: [false; 4],
            listed: false,
            state: WalkState::Asking,
        }
    }
}

impl HandleWalk {
    /// The request the host sends first: `04 01 00 ff ff`, the whole table.
    pub(crate) fn first() -> [u8; 5] {
        att::find_information_request(FIRST_HANDLE, LAST_HANDLE)
    }

    /// Takes one PDU from the pods while a request is out, as `05 01 07 00 03 28` lists handle 0x0007; the reply is the next request, and `None` with a state other than `Asking` is the end of the walk.
    pub(crate) fn on_pdu(&mut self, pdu: &[u8]) -> Option<[u8; 5]> {
        if self.state != WalkState::Asking {
            return None;
        }
        let pairs = match AttPdu::parse(pdu) {
            Ok(AttPdu::Information(pairs)) => pairs,
            // What the pods send unasked answers nothing, so the request is still out.
            Ok(
                AttPdu::Notification { .. }
                | AttPdu::Indication { .. }
                | AttPdu::Request { .. }
                | AttPdu::ReadResponse { .. }
                | AttPdu::WriteResponse
                | AttPdu::Other { .. },
            ) => return None,
            // An error response answers the request and is how a table ends.
            Ok(AttPdu::Error { .. }) => {
                self.state = WalkState::Ended;
                return None;
            }
            // A reply to another request, or one that does not parse, may not be the answer, so the request may still be out.
            Ok(AttPdu::ByType(_)) | Err(_) => {
                self.state = WalkState::Lost;
                return None;
            }
        };
        let mut last = None;
        for pair in pairs {
            self.listed = true;
            for (needed, seen) in NEEDED.iter().zip(&mut self.seen) {
                *seen |= *needed == pair.handle;
            }
            last = Some(pair.handle);
        }
        // Each request starts past the one before it, so a reply whose last handle is behind the request is one the walk cannot follow.
        let Some(last) = last.filter(|last| *last >= self.start) else {
            self.state = WalkState::Lost;
            return None;
        };
        // No handle follows 0xffff, so a listing that reaches it is the end of the table.
        let Some(next) = last.checked_add(1) else {
            self.state = WalkState::Ended;
            return None;
        };
        if self.requests >= ARM_MAX_REQUESTS {
            self.state = WalkState::Cut;
            return None;
        }
        self.requests = self.requests.saturating_add(1);
        self.start = next;
        Some(att::find_information_request(next, LAST_HANDLE))
    }

    pub(crate) fn state(&self) -> WalkState {
        self.state
    }

    /// What the pods have listed so far; a walk that was given no answer is `NoListing`.
    pub(crate) fn result(&self) -> HandleCheck {
        if !self.listed {
            return HandleCheck::NoListing;
        }
        let needed = NEEDED.iter().zip(self.seen);
        let missing: Vec<u16> = needed
            .filter(|(_, seen)| !seen)
            .map(|(handle, _)| *handle)
            .collect();
        if missing.is_empty() {
            HandleCheck::Present
        } else {
            HandleCheck::Missing(missing)
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// Stands in for a vendor's 128 bit identifier; no real one is a run of counting bytes.
    const FAKE_LONG_UUID: [u8; 16] = [
        0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd, 0xfe,
        0xff,
    ];
    /// The opcode of the request the walk sends, which an error response to it repeats.
    const FIND_INFORMATION: u8 = 0x04;
    /// The longest packet the property test feeds, one receive buffer.
    const LONGEST_PACKET: usize = 4096;

    #[test]
    fn a_type_of_either_width_reads_most_significant_octet_first() {
        assert_eq!(TypeText(Uuid::Short(0x2803)).to_string(), "0x2803");
        let long = TypeText(Uuid::Long(&FAKE_LONG_UUID)).to_string();
        assert_eq!(long, "fffefdfcfbfaf9f8f7f6f5f4f3f2f1f0");
    }

    /// A Find Information reply that gives each handle the 16 bit type 0x2803: `05 01`, then `07 00 03 28` for handle 0x0007.
    fn listing(handles: &[u16]) -> Vec<u8> {
        let mut pdu = vec![0x05, 0x01];
        for handle in handles {
            pdu.extend(handle.to_le_bytes());
            pdu.extend([0x03, 0x28]);
        }
        pdu
    }

    /// A Find Information reply with one handle of a 128 bit type, which is all such a reply carries when its neighbours have 16 bit types.
    fn long_listing(handle: u16) -> Vec<u8> {
        let mut pdu = vec![0x05, 0x02];
        pdu.extend(handle.to_le_bytes());
        pdu.extend(FAKE_LONG_UUID);
        pdu
    }

    fn handles(first: u16, last: u16) -> Vec<u16> {
        (first..=last).collect()
    }

    /// The error response that ends a table: no attribute from `start` on.
    fn not_found(start: u16) -> [u8; 5] {
        att::error_response(FIND_INFORMATION, start, att::ATTRIBUTE_NOT_FOUND)
    }

    /// The first handle of a request of the walk.
    fn start_of(request: [u8; 5]) -> u16 {
        let [opcode, start_low, start_high, end_low, end_high] = request;
        assert_eq!((opcode, end_low, end_high), (FIND_INFORMATION, 0xff, 0xff));
        u16::from_le_bytes([start_low, start_high])
    }

    #[test]
    fn the_walk_continues_from_the_last_handle_and_ends_on_not_found() {
        assert_eq!(HandleWalk::first(), [0x04, 0x01, 0x00, 0xff, 0xff]);
        let mut walk = HandleWalk::default();
        let next = walk.on_pdu(&listing(&handles(0x0001, 0x0008)));
        assert_eq!(next, Some([0x04, 0x09, 0x00, 0xff, 0xff]));
        // A reply that lists only the handle the request started at still moves the walk on by one.
        let next = walk.on_pdu(&long_listing(0x0009));
        assert_eq!(next, Some([0x04, 0x0a, 0x00, 0xff, 0xff]));
        let next = walk.on_pdu(&listing(&handles(0x000a, 0x0010)));
        assert_eq!(next, Some([0x04, 0x11, 0x00, 0xff, 0xff]));
        assert_eq!(walk.state(), WalkState::Asking);
        assert_eq!(walk.result(), HandleCheck::Present);

        assert_eq!(walk.on_pdu(&not_found(0x0011)), None);
        assert_eq!(walk.state(), WalkState::Ended);
        assert_eq!(walk.result(), HandleCheck::Present);
        // A walk that is over asks nothing more and learns nothing more.
        assert_eq!(walk.on_pdu(&listing(&handles(0x0011, 0x0012))), None);
        assert_eq!(walk.state(), WalkState::Ended);
    }

    #[test]
    fn the_declarations_request_covers_the_whole_table_once() {
        let request = declarations_request();
        assert_eq!(request, [0x08, 0x01, 0x00, 0xff, 0xff, 0x03, 0x28]);
    }

    #[test]
    fn the_walk_reports_a_missing_handle() {
        for (listed, missing, said) in [
            (
                vec![0x0001, 0x000a, 0x000d, 0x0010],
                vec![0x0007],
                "missing 0x0007",
            ),
            (
                vec![0x0007, 0x0008, 0x000d, 0x0010],
                vec![0x000a],
                "missing 0x000a",
            ),
            (
                vec![0x0007, 0x000a, 0x000c, 0x0010],
                vec![0x000d],
                "missing 0x000d",
            ),
            (
                vec![0x0007, 0x000a, 0x000d, 0x0011],
                vec![0x0010],
                "missing 0x0010",
            ),
            (
                vec![0x0002, 0x0007, 0x000d],
                vec![0x000a, 0x0010],
                "missing 0x000a 0x0010",
            ),
            (
                vec![0x0001],
                vec![0x0007, 0x000a, 0x000d, 0x0010],
                "missing 0x0007 0x000a 0x000d 0x0010",
            ),
        ] {
            let mut walk = HandleWalk::default();
            assert!(walk.on_pdu(&listing(&listed)).is_some(), "{listed:04x?}");
            let after = *listed.last().unwrap() + 1;
            assert_eq!(walk.on_pdu(&not_found(after)), None);
            let check = walk.result();
            assert_eq!(check, HandleCheck::Missing(missing), "{listed:04x?}");
            assert_eq!(check.to_string(), said);
        }
        assert_eq!(HandleCheck::Present.to_string(), "present");
        assert_eq!(HandleCheck::NoListing.to_string(), "no listing");
    }

    #[test]
    fn no_answer_is_no_listing() {
        let walk = HandleWalk::default();
        assert_eq!(walk.state(), WalkState::Asking);
        assert_eq!(walk.result(), HandleCheck::NoListing);

        // An error to the first request lists nothing either, whatever its code.
        for code in [att::ATTRIBUTE_NOT_FOUND, att::REQUEST_NOT_SUPPORTED] {
            let mut walk = HandleWalk::default();
            let refused = att::error_response(FIND_INFORMATION, FIRST_HANDLE, code);
            assert_eq!(walk.on_pdu(&refused), None);
            assert_eq!(walk.state(), WalkState::Ended);
            assert_eq!(walk.result(), HandleCheck::NoListing);
        }
    }

    #[test]
    fn a_listing_that_ends_at_0xffff_does_not_wrap() {
        let mut walk = HandleWalk::default();
        assert_eq!(walk.on_pdu(&listing(&[0xfffe, 0xffff])), None);
        assert_eq!(walk.state(), WalkState::Ended);
        assert_eq!(walk.result(), HandleCheck::Missing(NEEDED.to_vec()));

        let mut walk = HandleWalk::default();
        let next = walk.on_pdu(&listing(&[0xfffe]));
        assert_eq!(next, Some([0x04, 0xff, 0xff, 0xff, 0xff]));
        assert_eq!(walk.on_pdu(&listing(&[0xffff])), None);
        assert_eq!(walk.state(), WalkState::Ended);
    }

    #[test]
    fn a_peer_repeating_one_reply_cannot_keep_the_walk_going() {
        let reply = listing(&handles(0x0001, 0x0003));
        let mut walk = HandleWalk::default();
        assert_eq!(walk.on_pdu(&reply).map(start_of), Some(0x0004));
        assert_eq!(walk.on_pdu(&reply), None);
        assert_eq!(walk.state(), WalkState::Lost);

        // A reply that ends before the handle the request started at is behind the walk, wherever it began.
        let mut walk = HandleWalk::default();
        let next = walk.on_pdu(&listing(&handles(0x0001, 0x0020)));
        assert_eq!(next.map(start_of), Some(0x0021));
        assert_eq!(walk.on_pdu(&listing(&[0x0030, 0x0020])), None);
        assert_eq!(walk.state(), WalkState::Lost);
    }

    /// A walk fed one attribute a reply from handle 1 on, and the requests it had sent when it stopped asking.
    fn walk_of_single_replies(attributes: u16) -> (HandleWalk, u32) {
        let mut walk = HandleWalk::default();
        let mut requests = 1_u32;
        for handle in 1..=attributes {
            if walk.on_pdu(&listing(&[handle])).is_some() {
                requests = requests.saturating_add(1);
            }
        }
        (walk, requests)
    }

    #[test]
    fn a_table_that_takes_every_request_the_walk_may_send_is_read_to_its_end() {
        let attributes = u16::try_from(ARM_MAX_REQUESTS - 1).unwrap();
        let (mut walk, requests) = walk_of_single_replies(attributes);
        assert_eq!(requests, ARM_MAX_REQUESTS);
        assert_eq!(walk.state(), WalkState::Asking);
        assert_eq!(walk.on_pdu(&not_found(attributes + 1)), None);
        assert_eq!(walk.state(), WalkState::Ended);
        assert_eq!(walk.result(), HandleCheck::Present);
    }

    #[test]
    fn a_table_one_request_longer_cuts_the_walk_and_no_request_follows() {
        let attributes = u16::try_from(ARM_MAX_REQUESTS).unwrap();
        let (mut walk, requests) = walk_of_single_replies(attributes);
        assert_eq!(requests, ARM_MAX_REQUESTS);
        assert_eq!(walk.state(), WalkState::Cut);
        assert_eq!(walk.result(), HandleCheck::Present);
        // A walk that was cut asks nothing more, whatever arrives.
        assert_eq!(walk.on_pdu(&listing(&[attributes + 1])), None);
        assert_eq!(walk.on_pdu(&not_found(attributes + 2)), None);
        assert_eq!(walk.state(), WalkState::Cut);
    }

    #[test]
    fn a_reply_that_is_no_listing_ends_the_walk() {
        let empty_list = vec![0x05, 0x01];
        let unknown_format = vec![0x05, 0x03, 0x07, 0x00, 0x03, 0x28];
        let cut_notification = vec![0x1b, 0x0d];
        let declarations = vec![0x09, 0x07, 0x06, 0x00, 0x04, 0x07, 0x00, 0x2a, 0x2b];
        let cut_error = vec![0x01, 0x04, 0x01];
        for reply in [
            empty_list,
            unknown_format,
            cut_notification,
            declarations,
            cut_error,
            Vec::new(),
        ] {
            let mut walk = HandleWalk::default();
            assert!(walk.on_pdu(&listing(&[0x0007])).is_some());
            assert_eq!(walk.on_pdu(&reply), None, "{reply:02x?}");
            assert_eq!(walk.state(), WalkState::Lost, "{reply:02x?}");
            assert_eq!(
                walk.result(),
                HandleCheck::Missing(vec![0x000a, 0x000d, 0x0010])
            );
        }
    }

    #[test]
    fn what_the_pods_send_unasked_leaves_the_request_out() {
        let notification = vec![0x1b, 0x0d, 0x00, 0x01, 0x00];
        let indication = vec![0x1d, 0x21, 0x00, 0x01];
        let request = vec![0x02, 0x00, 0x02];
        let other = vec![0x03, 0x00, 0x02];
        let mut walk = HandleWalk::default();
        for unasked in [notification, indication, request, other] {
            assert_eq!(walk.on_pdu(&unasked), None, "{unasked:02x?}");
            assert_eq!(walk.state(), WalkState::Asking, "{unasked:02x?}");
            assert_eq!(walk.result(), HandleCheck::NoListing);
        }
        let next = walk.on_pdu(&listing(&NEEDED));
        assert_eq!(next.map(start_of), Some(0x0011));
        assert_eq!(walk.result(), HandleCheck::Present);
    }

    /// Any bytes at all, and listings of a few handles, which arbitrary bytes almost never form.
    fn any_pdu() -> impl Strategy<Value = Vec<u8>> {
        prop_oneof![
            proptest::collection::vec(any::<u8>(), 0..=LONGEST_PACKET),
            proptest::collection::vec(any::<u16>(), 1..6).prop_map(|handles| listing(&handles)),
            (any::<u16>(), 0_u16..4)
                .prop_map(|(first, more)| { listing(&handles(first, first.saturating_add(more))) }),
        ]
    }

    proptest! {
        #[test]
        fn every_request_starts_past_the_one_before_whatever_arrives(
            pdus in proptest::collection::vec(any_pdu(), 0..24),
        ) {
            let mut walk = HandleWalk::default();
            let mut start = start_of(HandleWalk::first());
            for pdu in &pdus {
                let was = walk.state();
                if let Some(request) = walk.on_pdu(pdu) {
                    prop_assert_eq!(was, WalkState::Asking);
                    prop_assert!(start_of(request) > start);
                    start = start_of(request);
                }
                // A walk never comes back from its end.
                prop_assert!(was == WalkState::Asking || walk.state() == was);
            }
            let check = walk.result();
            if let HandleCheck::Missing(missing) = &check {
                prop_assert!(!missing.is_empty() && missing.is_sorted());
                prop_assert!(missing.iter().all(|handle| NEEDED.contains(handle)));
            }
        }
    }
}
