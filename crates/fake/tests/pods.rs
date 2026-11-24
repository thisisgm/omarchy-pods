//! Support tests: the fake pods over a socket pair; no user path yet.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod support;

#[cfg(test)]
#[path = "support/arbitrary.rs"]
mod arbitrary;

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use pods_fake::{Fault, Pods, Profile};
    use pods_samples::{
        CONVERSATION_AWARENESS, Cell, NOISE, ONE_BUD_ANC, Placed, battery, battery_of,
        burst_others, burst_settings, ear, ear_of, features_ack, first_burst, handshake_ack, host,
        metadata_of, setting,
    };
    use pods_sys::l2cap::{Channel, ChannelError, Received};
    use proptest::prelude::*;
    use rustix::io::Errno;

    use super::arbitrary;
    use super::support::{
        BUFFER_LEN, BURST_LEN, QUIET, REAL_TIME_SLACK, RECV_TIMEOUT, ask, assert_quiet, open,
        opening, recv, recv_many,
    };

    /// Far more packets than a socket pair's queue holds, so the fake meets a full send buffer.
    const FLOOD: usize = 2000;
    /// Far past an `AF_UNIX` send buffer (208 KiB by default), so the kernel refuses the SDU with `EMSGSIZE`.
    const REFUSED_SDU_LEN: usize = 4 << 20;
    /// Past the 672 byte default L2CAP MTU and a 4096 byte receive buffer: an SDU no real pods send.
    const OVERSIZE_SDU_LEN: usize = 5000;

    fn assert_closed(host: &Channel) {
        let got = host.recv(&mut vec![0; BUFFER_LEN]).unwrap();
        assert_eq!(got, Received::Closed, "closed within {RECV_TIMEOUT:?}");
    }

    fn finish(pods: Pods, host: Channel) {
        drop(host);
        pods.finish().unwrap();
    }

    #[test]
    fn the_opening_is_answered_step_by_step() {
        let pods = Pods::new(Profile::default());
        let host = open(&pods);
        assert_quiet(&host);
        ask(&host, &host::handshake());
        assert_eq!(recv(&host), handshake_ack());
        assert_quiet(&host);
        ask(&host, &host::set_specific_features());
        assert_eq!(recv(&host), features_ack());
        assert_quiet(&host);
        ask(&host, &host::request_notifications());
        assert_eq!(recv_many(&host, BURST_LEN), first_burst()[2..]);
        assert_quiet(&host);
        finish(pods, host);
    }

    #[test]
    fn the_burst_carries_the_profile() {
        let left = Cell {
            level: 10,
            charging: false,
        };
        let right = Cell {
            level: 20,
            charging: true,
        };
        let case = Cell {
            level: 30,
            charging: true,
        };
        let pods = Pods::new(Profile {
            name: "Synthetic Pods".to_owned(),
            model_number: "A0000".to_owned(),
            left,
            right,
            case,
            primary: Placed::OutOfEar,
            secondary: Placed::InCase,
            noise: 3,
            keys: None,
        });
        let host = open(&pods);
        let mut expected = vec![
            battery_of(left, right, case),
            ear_of(Placed::OutOfEar, Placed::InCase),
            metadata_of("Synthetic Pods", "A0000"),
        ];
        expected.extend(burst_settings(&[(NOISE, 3)]));
        expected.extend(burst_others());
        assert_eq!(opening(&host), expected);
        finish(pods, host);
    }

    #[test]
    fn a_host_setting_is_applied_and_reported_back() {
        let pods = Pods::new(Profile::default());
        let host = open(&pods);
        opening(&host);
        ask(&host, &setting(CONVERSATION_AWARENESS, 1));
        assert_eq!(recv(&host), setting(CONVERSATION_AWARENESS, 1));
        ask(&host, &setting(NOISE, 2));
        assert_eq!(recv(&host), setting(NOISE, 2));
        ask(&host, &host::request_notifications());
        assert!(recv_many(&host, BURST_LEN).contains(&setting(NOISE, 2)));
        finish(pods, host);
    }

    #[test]
    fn an_unchanged_host_setting_is_reported_back_again() {
        let pods = Pods::new(Profile::default());
        let host = open(&pods);
        opening(&host);
        for _ in 0..2 {
            ask(&host, &setting(NOISE, 1));
            assert_eq!(recv(&host), setting(NOISE, 1));
        }
        finish(pods, host);
    }

    #[test]
    fn noise_is_ignored_with_no_bud_in_an_ear() {
        let pods = Pods::new(Profile {
            primary: Placed::OutOfEar,
            secondary: Placed::OutOfEar,
            ..Profile::default()
        });
        let host = open(&pods);
        opening(&host);
        ask(&host, &setting(NOISE, 2));
        assert_quiet(&host);
        ask(&host, &host::request_notifications());
        assert!(recv_many(&host, BURST_LEN).contains(&setting(NOISE, 1)));
        pods.set_ear(Placed::OutOfEar, Placed::InEar);
        assert_eq!(recv(&host), ear_of(Placed::OutOfEar, Placed::InEar));
        ask(&host, &setting(NOISE, 2));
        assert_eq!(recv(&host), setting(NOISE, 2));
        finish(pods, host);
    }

    #[test]
    fn a_change_made_by_the_test_is_reported_once() {
        let pods = Pods::new(Profile::default());
        let host = open(&pods);
        opening(&host);
        pods.set_ear(Placed::InCase, Placed::OutOfEar);
        assert_eq!(recv(&host), ear_of(Placed::InCase, Placed::OutOfEar));
        assert_quiet(&host);
        let low = Cell {
            level: 5,
            charging: true,
        };
        pods.set_battery(low, low, low);
        assert_eq!(recv(&host), battery_of(low, low, low));
        assert_quiet(&host);
        pods.set_setting(ONE_BUD_ANC, 2);
        assert_eq!(recv(&host), setting(ONE_BUD_ANC, 2));
        assert_quiet(&host);
        ask(&host, &host::request_notifications());
        let burst = recv_many(&host, BURST_LEN);
        assert_eq!(
            burst[..2],
            [
                battery_of(low, low, low),
                ear_of(Placed::InCase, Placed::OutOfEar)
            ]
        );
        finish(pods, host);
    }

    #[test]
    fn raw_bytes_go_out_as_they_are() {
        let pods = Pods::new(Profile::default());
        let host = open(&pods);
        let oversize = vec![0x5a; OVERSIZE_SDU_LEN];
        pods.send(&oversize);
        assert_eq!(recv(&host), oversize);
        assert_quiet(&host);
        finish(pods, host);
    }

    #[test]
    fn an_sdu_the_socket_refuses_fails_the_finish() {
        let pods = Pods::new(Profile::default());
        let host = open(&pods);
        pods.send(&vec![0x5a; REFUSED_SDU_LEN]);
        assert_closed(&host);
        drop(host);
        let failure = pods.finish().unwrap_err();
        assert!(
            failure.starts_with("fake-pods thread 0: could not send"),
            "{failure}"
        );
    }

    #[test]
    fn a_host_that_leaves_packets_unread_does_not_fail_the_finish() {
        let pods = Pods::new(Profile::default());
        pods.fault(Fault::ReportsBeforeAck);
        let host = open(&pods);
        assert_eq!(recv(&host), battery());
        ask(&host, &host::handshake());
        // The fake reads only after it has sent what it queued, so the ear report now waits unread.
        pods.wait_heard(&host::handshake(), 1, RECV_TIMEOUT)
            .unwrap();
        drop(host);
        pods.wait_closed(1, RECV_TIMEOUT).unwrap();
        pods.finish().unwrap();
    }

    #[test]
    fn a_full_send_buffer_delays_packets_and_drops_none() {
        let pods = Pods::new(Profile::default());
        let host = open(&pods);
        let packets: Vec<Vec<u8>> = (0..FLOOD).map(|n| n.to_le_bytes().to_vec()).collect();
        for packet in &packets {
            pods.send(packet);
        }
        assert_eq!(recv_many(&host, FLOOD), packets);
        finish(pods, host);
    }

    #[test]
    fn a_fault_holds_only_for_channels_opened_after_it() {
        let pods = Pods::new(Profile::default());
        let before = open(&pods);
        pods.fault(Fault::NeverAcknowledge);
        let after = open(&pods);
        for host in [&before, &after] {
            ask(host, &host::handshake());
        }
        assert_eq!(recv(&before), handshake_ack());
        pods.wait_heard(&host::handshake(), 2, RECV_TIMEOUT)
            .unwrap();
        assert_quiet(&after);
        drop(before);
        finish(pods, after);
    }

    #[test]
    fn never_acknowledge_sends_nothing() {
        let pods = Pods::new(Profile::default());
        pods.fault(Fault::NeverAcknowledge);
        let host = open(&pods);
        let frames = [
            host::handshake(),
            host::set_specific_features(),
            host::request_notifications(),
            setting(CONVERSATION_AWARENESS, 2),
        ];
        for frame in &frames {
            ask(&host, frame);
        }
        let last = frames.last().unwrap();
        pods.wait_heard(last, 1, RECV_TIMEOUT).unwrap();
        pods.set_ear(Placed::OutOfEar, Placed::OutOfEar);
        assert_quiet(&host);
        let heard: Vec<Vec<u8>> = pods.heard().into_iter().map(|heard| heard.sdu).collect();
        assert_eq!(heard, frames);
        finish(pods, host);
    }

    #[test]
    fn close_after_n_packets_closes_there() {
        let pods = Pods::new(Profile::default());
        for (closed, limit) in [0, 1, 10].into_iter().enumerate() {
            pods.fault(Fault::CloseAfter(limit));
            let host = open(&pods);
            let mut got = Vec::new();
            if limit > 0 {
                ask(&host, &host::handshake());
                got.push(recv(&host));
            }
            if limit > 1 {
                ask(&host, &host::set_specific_features());
                got.push(recv(&host));
                ask(&host, &host::request_notifications());
                got.extend(recv_many(&host, limit - 2));
            }
            assert_closed(&host);
            assert_eq!(got, first_burst()[..limit], "CloseAfter({limit})");
            pods.wait_closed(closed + 1, RECV_TIMEOUT).unwrap();
        }
        pods.finish().unwrap();
    }

    #[test]
    fn reports_come_at_once_and_no_acknowledgement_follows() {
        let pods = Pods::new(Profile::default());
        pods.fault(Fault::ReportsBeforeAck);
        let host = open(&pods);
        assert_eq!(recv_many(&host, 2), [battery(), ear()]);
        for frame in [
            host::handshake(),
            host::set_specific_features(),
            host::request_notifications(),
        ] {
            ask(&host, &frame);
            assert_quiet(&host);
        }
        finish(pods, host);
    }

    #[test]
    fn the_battery_is_withheld_on_every_request() {
        let pods = Pods::new(Profile::default());
        pods.fault(Fault::WithholdBattery);
        let host = open(&pods);
        ask(&host, &host::handshake());
        assert_eq!(recv(&host), handshake_ack());
        for _ in 0..2 {
            ask(&host, &host::request_notifications());
            assert_eq!(recv_many(&host, BURST_LEN - 1), first_burst()[3..]);
            assert_quiet(&host);
        }
        let low = Cell {
            level: 5,
            charging: false,
        };
        pods.set_battery(low, low, low);
        assert_quiet(&host);
        finish(pods, host);
    }

    #[test]
    fn never_confirm_ignores_host_settings() {
        let pods = Pods::new(Profile::default());
        pods.fault(Fault::NeverConfirm);
        let host = open(&pods);
        opening(&host);
        ask(&host, &setting(CONVERSATION_AWARENESS, 2));
        ask(&host, &setting(NOISE, 3));
        assert_quiet(&host);
        ask(&host, &host::request_notifications());
        assert!(recv_many(&host, BURST_LEN).contains(&setting(NOISE, 1)));
        pods.set_setting(CONVERSATION_AWARENESS, 2);
        assert_eq!(recv(&host), setting(CONVERSATION_AWARENESS, 2));
        finish(pods, host);
    }

    #[test]
    fn a_channel_that_stopped_reading_fails_the_host_write() {
        /// Far more frames than an `AF_UNIX` send buffer holds.
        const WRITES_MAX: usize = 100_000;
        let pods = Pods::new(Profile::default());
        let host = open(&pods);
        opening(&host);
        pods.stop_reading();
        let frame = setting(NOISE, 2);
        let failed = (0..WRITES_MAX).find_map(|_| host.try_send(&frame).err());
        assert_eq!(failed, Some(ChannelError::Send(Errno::AGAIN)));
        // At most the one frame a receive already waiting took; a reading fake drains the whole buffer.
        assert!(pods.wait_heard(&frame, 2, QUIET).is_err());
        finish(pods, host);
    }

    #[test]
    fn everything_heard_is_kept_in_order_with_its_time() {
        let pods = Pods::new(Profile::default());
        let host = open(&pods);
        let before = Instant::now();
        let frames = [
            host::handshake(),
            vec![0xde, 0xad],
            host::set_specific_features(),
        ];
        for frame in &frames {
            ask(&host, frame);
        }
        assert_eq!(recv_many(&host, 2), [handshake_ack(), features_ack()]);
        let after = Instant::now();
        let heard = pods.heard();
        let sdus: Vec<Vec<u8>> = heard.iter().map(|heard| heard.sdu.clone()).collect();
        assert_eq!(sdus, frames);
        let times: Vec<Instant> = heard.iter().map(|heard| heard.at).collect();
        assert!(times.is_sorted(), "{times:?}");
        assert!(
            times.iter().all(|at| (before..=after).contains(at)),
            "{times:?} outside {before:?}..={after:?}"
        );
        finish(pods, host);
    }

    #[test]
    fn wait_heard_ends_at_its_deadline() {
        let pods = Pods::new(Profile::default());
        let host = open(&pods);
        ask(&host, &host::handshake());
        assert_eq!(recv(&host), handshake_ack());
        let deadline = Duration::from_millis(100);
        for (sdu, count) in [(vec![0x01, 0x02], 1), (host::handshake(), 2)] {
            let waited = pods.wait_heard(&sdu, count, deadline).unwrap_err().waited;
            assert!(
                waited >= deadline,
                "waited {waited:?}, at least {deadline:?}"
            );
            assert!(
                waited <= deadline + REAL_TIME_SLACK,
                "waited {waited:?}, at most {deadline:?} and {REAL_TIME_SLACK:?}"
            );
        }
        assert_eq!(
            pods.wait_heard(&host::handshake(), 1, deadline)
                .unwrap()
                .len(),
            1
        );
        finish(pods, host);
    }

    #[test]
    fn hang_up_closes_the_channel_and_ends_the_thread() {
        let pods = Pods::new(Profile::default());
        let host = open(&pods);
        opening(&host);
        let before = Instant::now();
        pods.hang_up();
        assert_closed(&host);
        let closed = pods.wait_closed(1, RECV_TIMEOUT).unwrap();
        assert_eq!(closed.len(), 1);
        assert!(closed[0] >= before);
        pods.finish().unwrap();
    }

    #[test]
    fn wait_hung_up_waits_for_the_hung_up_channel_and_not_for_one_opened_since() {
        let pods = Pods::new(Profile::default());
        let hung_up = open(&pods);
        pods.hang_up();
        let opened_since = open(&pods);
        pods.wait_hung_up(RECV_TIMEOUT).unwrap();
        assert_eq!(pods.wait_closed(0, Duration::ZERO).unwrap().len(), 1);
        assert_closed(&hung_up);
        ask(&opened_since, &host::handshake());
        assert_eq!(recv(&opened_since), handshake_ack());
        finish(pods, opened_since);
    }

    #[test]
    fn a_clone_that_serves_after_the_finish_starts_no_thread() {
        let pods = Pods::new(Profile::default());
        let late = pods.clone();
        pods.finish().unwrap();
        let host = open(&late);
        assert_closed(&host);
        assert_eq!(late.most_open_at_once(), 0);
    }

    #[test]
    fn two_channels_never_count_as_one() {
        let pods = Pods::new(Profile::default());
        let first = open(&pods);
        let second = open(&pods);
        for host in [&first, &second] {
            ask(host, &host::handshake());
            assert_eq!(recv(host), handshake_ack());
        }
        pods.set_ear(Placed::InCase, Placed::InCase);
        for host in [&first, &second] {
            assert_eq!(recv(host), ear_of(Placed::InCase, Placed::InCase));
        }
        assert_eq!(pods.most_open_at_once(), 2);
        drop(first);
        pods.wait_closed(1, RECV_TIMEOUT).unwrap();
        let third = open(&pods);
        assert_eq!(pods.most_open_at_once(), 2);
        drop(second);
        finish(pods, third);
    }

    proptest! {
        #[test]
        fn arbitrary_bytes_from_the_host_never_stop_the_fake(sdus in arbitrary::sdus()) {
            let pods = Pods::new(Profile::default());
            let host = open(&pods);
            for sdu in &sdus {
                ask(&host, sdu);
            }
            ask(&host, &host::handshake());
            // A random SDU equal to an opening frame or a setting is answered first; at most a burst each.
            let most_replies = sdus.len().saturating_mul(BURST_LEN).saturating_add(1);
            let answered = (0..most_replies).any(|_| recv(&host) == handshake_ack());
            prop_assert!(answered);
            drop(host);
            prop_assert_eq!(pods.finish(), Ok(()));
        }
    }
}
