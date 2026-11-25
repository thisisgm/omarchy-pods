//! Support: the machine of the dictation service on a clock the test holds, with no socket.

use pods_samples::doap::{
    ATTRIBUTE_NOT_FOUND, CONFIGURATION, DECLARATION, FIND_INFORMATION_REQUEST,
    READ_BY_TYPE_REQUEST, READ_NOT_PERMITTED, READ_REQUEST, REASON_HOST, WRITE_NOT_PERMITTED,
    WRITE_REQUEST,
};

use super::*;

const CONTROL: usize = 1;
const ATT: usize = 2;
/// A second channel of each kind.
const OTHER_CONTROL: usize = 3;
const OTHER_ATT: usize = 4;
const TABLE: Handles = Handles::OBSERVED;
const NOTHING: [Vec<u8>; 0] = [];
/// The opcode of the MTU request, which stands for any request of the pods' own.
const MTU_REQUEST: u8 = 0x02;
/// The type of a primary service declaration, which no table here lists.
const PRIMARY_SERVICE: u16 = 0x2800;

fn ms(count: u64) -> Duration {
    Duration::from_millis(count)
}

fn subscribe(machine: &mut Machine, line: usize, table: &Handles, now: Instant) {
    machine.opened(line);
    for descriptor in table.cccds {
        let written = machine.on_att(line, &host::subscribe(descriptor), now);
        assert_eq!(written, [sample::WRITTEN]);
    }
}

/// A machine with every descriptor written on `ATT` and the mode set at the instant returned.
fn armed() -> (Machine, Instant) {
    let mut machine = Machine::new();
    let since = Instant::now();
    subscribe(&mut machine, ATT, &TABLE, since);
    machine.on_control(CONTROL, &setting(DOAP_MODE, 1), since);
    (machine, since)
}

/// Sends a START on `ATT` at `now` and returns everything due at once.
fn start(machine: &mut Machine, now: Instant) -> Vec<Vec<u8>> {
    let mut sent = machine.on_att(ATT, &host::start(&TABLE), now);
    sent.extend(machine.due(ATT, now));
    sent
}

#[test]
fn start_needs_settle_time_after_mode_and_mode_off_disables_it() {
    let (mut machine, since) = armed();
    let refusal = sample::stopped(&TABLE, REASON_UNASKED);
    let settled = since + ms(100);
    assert_eq!(start(&mut machine, since), [refusal]);
    assert_eq!(start(&mut machine, since + ms(99)), [refusal]);
    assert_eq!(start(&mut machine, settled)[0], sample::score(&TABLE));

    machine.on_att(ATT, &host::stop(&TABLE, REASON_HOST), settled);
    assert_eq!(start(&mut machine, settled)[0], sample::score(&TABLE));
    machine.on_att(ATT, &host::stop(&TABLE, REASON_HOST), settled);
    machine.on_control(CONTROL, &setting(DOAP_MODE, 1), settled);
    assert_eq!(start(&mut machine, settled), [refusal]);
    let settled_again = settled + ms(100);
    let sent = start(&mut machine, settled_again);
    assert_eq!(sent[0], sample::score(&TABLE));
    assert_eq!(sent[1..], [tone(0, 0..5), tone(1, 5..10)]);
    machine.on_att(ATT, &host::stop(&TABLE, REASON_HOST), settled_again);
    machine.on_control(CONTROL, &setting(DOAP_MODE, 2), settled_again);
    assert!(machine.mic.is_none());
    assert_eq!(start(&mut machine, settled_again + ms(100)), [refusal]);
}

#[test]
fn selecting_codec_before_subscriptions_does_not_arm() {
    let mut machine = Machine::new();
    let now = Instant::now();
    machine.opened(ATT);

    machine.on_att(ATT, &host::select_codec(&TABLE), now);
    assert_eq!(machine.selections, 0);
    for descriptor in TABLE.cccds {
        machine.on_att(ATT, &host::subscribe(descriptor), now);
    }
    assert_eq!(machine.selections, 0);
    machine.on_att(ATT, &host::select_codec(&TABLE), now);
    assert_eq!(
        machine.selections, 1,
        "a selection after the subscriptions counts"
    );
}

/// One notification of `fixture` frames: each counter with the place the microphone heard it at.
fn audio(fixture: &[&'static [u8]], packet: u8, heard: &[(u16, usize)]) -> Vec<u8> {
    let frames: Vec<(u16, &[u8])> = heard
        .iter()
        .map(|(counter, place)| (*counter, fixture[place.checked_rem(fixture.len()).unwrap()]))
        .collect();
    sample::audio(&TABLE, packet, &frames)
}

/// One notification of the tone whose frames count from the moment the mode was set.
fn tone(packet: u8, counters: std::ops::Range<u16>) -> Vec<u8> {
    let fixture: Vec<&[u8]> = opus::tone().collect();
    let heard: Vec<(u16, usize)> = counters
        .map(|counter| (counter, usize::from(counter)))
        .collect();
    audio(&fixture, packet, &heard)
}

#[test]
fn the_walk_is_answered_two_declarations_a_reply_until_the_table_ends() {
    let (mut machine, now) = armed();
    let declared = TABLE.declared();
    for (from, listed) in [(0x0001, 0..2), (0x0005, 2..4), (0x000a, 4..6)] {
        let reply = machine.on_att(ATT, &host::declarations_from(from), now);
        assert_eq!(
            reply,
            [sample::declarations(&declared[listed])],
            "{from:#06x}"
        );
    }
    let end = sample::error(READ_BY_TYPE_REQUEST, 0x0010, ATTRIBUTE_NOT_FOUND);
    assert_eq!(
        machine.on_att(ATT, &host::declarations_from(0x0010), now),
        [end]
    );
    // A walk for the primary services, which this table does not list.
    let mut services = host::declarations_from(0x0001);
    services[5..].copy_from_slice(&PRIMARY_SERVICE.to_le_bytes());
    let none = sample::error(READ_BY_TYPE_REQUEST, 0x0001, ATTRIBUTE_NOT_FOUND);
    assert_eq!(machine.on_att(ATT, &services, now), [none]);
}

#[test]
fn the_types_are_listed_one_width_a_reply() {
    let (mut machine, now) = armed();
    let [.., stop, _, _] = TABLE.declared();
    let replies = [
        (0x0002, sample::short_types(&[(0x0002, DECLARATION)])),
        (
            0x0008,
            sample::short_types(&[(0x0008, CONFIGURATION), (0x0009, DECLARATION)]),
        ),
        (0x000a, sample::long_types(&[(0x000a, stop.uuid)])),
        (0x0011, sample::short_types(&[(0x0011, CONFIGURATION)])),
        (
            0x0012,
            sample::error(FIND_INFORMATION_REQUEST, 0x0012, ATTRIBUTE_NOT_FOUND).to_vec(),
        ),
    ];
    for (from, reply) in replies {
        let asked = host::types_between(from, u16::MAX);
        assert_eq!(machine.on_att(ATT, &asked, now), [reply], "{from:#06x}");
    }
    let one = host::types_between(0x0008, 0x0008);
    let only = sample::short_types(&[(0x0008, CONFIGURATION)]);
    assert_eq!(machine.on_att(ATT, &one, now), [only]);
}

#[test]
fn the_codec_list_is_read_and_any_other_read_or_write_is_refused() {
    let (mut machine, now) = armed();
    let codecs = machine.on_att(ATT, &host::read(TABLE.supported_codecs), now);
    assert_eq!(codecs, [sample::CODECS]);
    let read = machine.on_att(ATT, &host::read(TABLE.start), now);
    assert_eq!(
        read,
        [sample::error(READ_REQUEST, TABLE.start, READ_NOT_PERMITTED)]
    );
    let write = machine.on_att(ATT, &host::subscribe(TABLE.start), now);
    assert_eq!(
        write,
        [sample::error(
            WRITE_REQUEST,
            TABLE.start,
            WRITE_NOT_PERMITTED
        )]
    );
    // A write command has no answer, and neither has what is no packet of the protocol.
    assert_eq!(
        machine.on_att(ATT, &host::select_codec(&TABLE), now),
        NOTHING
    );
    assert_eq!(machine.on_att(ATT, &[], now), NOTHING);
}

#[test]
fn the_shifted_table_stands_32_handles_up_and_the_absent_one_holds_nothing() {
    let (mut machine, now) = armed();
    machine.table = Table::Shifted;
    let shifted = TABLE.shifted(SHIFT);
    assert_eq!(shifted.start, 0x0027);
    let first = machine.on_att(ATT, &host::declarations_from(0x0001), now);
    assert_eq!(first, [sample::declarations(&shifted.declared()[..2])]);
    // A START on the handle of the observed table is no START here.
    assert_eq!(machine.on_att(ATT, &host::start(&TABLE), now), NOTHING);
    assert_eq!(machine.starts, 0);
    subscribe(&mut machine, ATT, &shifted, now);
    let refused = machine.on_att(ATT, &host::start(&shifted), now + ms(100));
    assert_eq!(refused, NOTHING);
    let frames: Vec<(u16, &[u8])> = (0..).zip(opus::tone().take(5)).collect();
    let answered = [sample::score(&shifted), sample::audio(&shifted, 0, &frames)];
    assert_eq!(machine.due(ATT, now + ms(100)), answered);
    assert_eq!(machine.starts, 1);

    machine.table = Table::Absent;
    let none = sample::error(READ_BY_TYPE_REQUEST, 0x0001, ATTRIBUTE_NOT_FOUND);
    assert_eq!(
        machine.on_att(ATT, &host::declarations_from(0x0001), now),
        [none]
    );
    let none = sample::error(FIND_INFORMATION_REQUEST, 0x0001, ATTRIBUTE_NOT_FOUND);
    let types = host::types_between(0x0001, u16::MAX);
    assert_eq!(machine.on_att(ATT, &types, now), [none]);
    let read = host::read(TABLE.supported_codecs);
    let refused = sample::error(READ_REQUEST, TABLE.supported_codecs, READ_NOT_PERMITTED);
    assert_eq!(machine.on_att(ATT, &read, now), [refused]);
    // A START is no START on a table that holds none, so it is not counted.
    assert_eq!(machine.on_att(ATT, &host::start(&TABLE), now), NOTHING);
    assert_eq!(machine.starts, 1);
    assert_eq!(machine.due(ATT, now + ms(200)), NOTHING);
    // With nothing ever due the channel sleeps, though the stream of the shifted table still runs.
    assert_eq!(machine.wait(ATT, now + ms(200)), None);
    machine.end_unasked();
    machine.audio_unasked(1, now + ms(200));
    assert_eq!(machine.due(ATT, now + ms(200)), NOTHING);
    machine.table = Table::Shifted;
    assert_eq!(machine.wait(ATT, now + ms(200)), Some(Duration::ZERO));
}

#[test]
fn a_start_is_refused_without_the_mode_and_answered_with_the_score_and_the_burst_with_it() {
    let mut machine = Machine::new();
    let now = Instant::now();
    subscribe(&mut machine, ATT, &TABLE, now);
    let refusal = sample::stopped(&TABLE, REASON_UNASKED);
    assert_eq!(start(&mut machine, now), [refusal]);
    // Another setting is not the mode, and the mode on a second channel changes nothing.
    machine.on_control(CONTROL, &setting(DOAP_MODE, 2), now);
    assert!(machine.mic.is_none());
    machine.on_control(CONTROL, &setting(DOAP_MODE, 1), now);
    machine.on_control(OTHER_CONTROL, &setting(DOAP_MODE, 1), now + ms(500));
    assert_eq!(machine.wait(ATT, now), None);

    // Two seconds are 100 frames, of which the last 63 are the burst: twelve notifications of five and one of three.
    let sent = start(&mut machine, now + ms(2000));
    let mut expected = vec![sample::score(&TABLE)];
    expected.extend((0..12).map(|packet| {
        let from = 37 + u16::from(packet) * 5;
        tone(packet, from..from + 5)
    }));
    expected.push(tone(12, 97..100));
    assert_eq!(sent, expected);
    assert_eq!(machine.starts, 2);
    // A START while the stream runs changes nothing.
    assert_eq!(start(&mut machine, now + ms(2000)), NOTHING);
    assert_eq!(machine.due(ATT, now + ms(2020)), [tone(13, 100..101)]);
}

#[test]
fn the_microphone_keeps_80_frames_and_a_burst_holds_no_more() {
    let (mut machine, since) = armed();
    machine.burst = (200, 40);
    let sent = start(&mut machine, since + ms(2000));
    let burst = [tone(0, 20..60), tone(1, 60..100)];
    assert_eq!(sent[1..], burst);
    // A burst of nothing a notification is sent a frame a notification.
    let (mut machine, since) = armed();
    machine.burst = (2, 0);
    let sent = start(&mut machine, since + ms(100));
    assert_eq!(sent[1..], [tone(0, 3..4), tone(1, 4..5)]);
}

#[test]
fn a_frame_is_due_every_20_ms_from_the_mode_and_never_before() {
    let (mut machine, since) = armed();
    machine.burst = (0, 5);
    // The start comes 5 ms into a frame, so the first frame after it is due 15 ms later.
    assert_eq!(
        start(&mut machine, since + ms(105)),
        [sample::score(&TABLE)]
    );
    assert_eq!(machine.wait(ATT, since + ms(105)), Some(ms(15)));
    assert_eq!(machine.due(ATT, since + ms(119)), NOTHING);
    assert_eq!(machine.due(ATT, since + ms(120)), [tone(0, 5..6)]);
    // One notification has no gap to another yet.
    assert_eq!(machine.largest_gap, None);
    // A late turn sends every frame heard meanwhile, each in its own notification.
    let late = [tone(1, 6..7), tone(2, 7..8), tone(3, 8..9)];
    assert_eq!(machine.due(ATT, since + ms(185)), late);
    assert_eq!(machine.wait(ATT, since + ms(185)), Some(ms(15)));
    assert_eq!(machine.largest_gap, Some(ms(65)));
    assert_eq!(machine.wait(OTHER_ATT, since + ms(185)), None);
    assert_eq!(machine.due(OTHER_ATT, since + ms(500)), NOTHING);
}

#[test]
fn the_starts_refused_are_counted_down_then_one_is_answered() {
    let (mut machine, since) = armed();
    let settled = since + MODE_SETTLE;
    machine.refusals = 2;
    machine.burst = (0, 5);
    let refusal = sample::stopped(&TABLE, REASON_UNASKED);
    for _ in 0..2 {
        assert_eq!(start(&mut machine, settled), [refusal]);
    }
    assert_eq!(start(&mut machine, settled), [sample::score(&TABLE)]);
    assert_eq!((machine.starts, machine.refusals), (3, 0));
}

#[test]
fn a_held_start_waits_for_its_release() {
    let (mut machine, since) = armed();
    machine.hold_start = true;
    machine.burst = (1, 1);
    assert_eq!(start(&mut machine, since + ms(100)), NOTHING);
    assert_eq!(machine.wait(ATT, since + ms(100)), None);
    // A second START while one waits changes nothing.
    assert_eq!(start(&mut machine, since + ms(150)), NOTHING);
    assert!(machine.run.is_none());
    machine.release_start(since + ms(300));
    assert_eq!(machine.wait(ATT, since + ms(300)), Some(Duration::ZERO));
    // The burst is what was heard before the release, not before the START.
    let answered = [sample::score(&TABLE), tone(0, 14..15)];
    assert_eq!(machine.due(ATT, since + ms(300)), answered);
    // With nothing held a release changes nothing.
    machine.release_start(since + ms(310));
    assert_eq!(machine.due(ATT, since + ms(310)), NOTHING);
    assert_eq!(machine.starts, 2);
}

#[test]
fn a_mark_is_heard_streaming_or_not_and_the_counter_starts_where_a_test_sets_it() {
    let (mut machine, since) = armed();
    let mark = opus::mark_onset();
    let tones: Vec<&[u8]> = opus::tone().collect();
    machine.hear(since + ms(40));
    (machine.marked, machine.next_counter) = (2, u16::MAX);
    machine.burst = (4, 4);
    let sent = start(&mut machine, since + ms(100));
    // The frames at places 2 and 3 are the mark, and they carry the counters set after place 1.
    let burst = [(1, tones[1]), (u16::MAX, mark), (0, mark), (1, tones[4])];
    assert_eq!(sent[1..], [sample::audio(&TABLE, 0, &burst)]);
    machine.marked = 1;
    assert_eq!(
        machine.due(ATT, since + ms(120)),
        [sample::audio(&TABLE, 1, &[(2, mark)])]
    );
    assert_eq!(
        machine.due(ATT, since + ms(140)),
        [audio(&tones, 2, &[(3, 6)])]
    );
}

#[test]
fn the_burst_sends_one_frame_twice_when_a_test_asks() {
    let (mut machine, since) = armed();
    machine.burst = (3, 5);
    machine.repeat = Some(1);
    let tones: Vec<&[u8]> = opus::tone().collect();
    let sent = start(&mut machine, since + ms(100));
    let twice = audio(&tones, 0, &[(2, 2), (3, 3), (3, 3), (4, 4)]);
    assert_eq!(sent[1..], [twice]);
    assert_eq!(machine.repeat, None);
    // An offset past the burst repeats nothing.
    let (mut machine, since) = armed();
    machine.burst = (3, 5);
    machine.repeat = Some(3);
    assert_eq!(start(&mut machine, since + ms(100))[1..], [tone(0, 2..5)]);
    // It is spent with its burst all the same, so no frame of the stream goes out twice.
    assert_eq!(machine.repeat, None);
    assert_eq!(machine.due(ATT, since + ms(120)), [tone(1, 5..6)]);
}

#[test]
fn a_lost_frame_takes_its_counter_and_a_garbled_one_keeps_it() {
    let (mut machine, since) = armed();
    machine.burst = (5, 2);
    (machine.lost, machine.garbled) = (3, 1);
    let tones: Vec<&[u8]> = opus::tone().collect();
    let mut garbled = tones[3].to_vec();
    garbled[0] = GARBLED_TOC;
    let sent = start(&mut machine, since + ms(100));
    // Frames 0, 1 and 2 are lost, so the first notification of two is never made.
    let after = [sample::audio(&TABLE, 0, &[(3, &garbled)]), tone(1, 4..5)];
    assert_eq!(sent[1..], after);
    assert_ne!(garbled, tones[3]);
    machine.lost = 1;
    assert_eq!(machine.due(ATT, since + ms(120)), NOTHING);
    assert_eq!(machine.due(ATT, since + ms(140)), [tone(2, 6..7)]);
}

#[test]
fn a_stall_holds_one_frame_and_the_frames_behind_it_go_out_with_it() {
    let (mut machine, since) = armed();
    machine.burst = (1, 1);
    assert_eq!(start(&mut machine, since + ms(100)).len(), 2);
    // The stall counts from the notification of the burst, made at 100 ms.
    machine.stall = Some(ms(100));
    assert_eq!(machine.wait(ATT, since + ms(100)), Some(ms(100)));
    assert_eq!(machine.due(ATT, since + ms(199)), NOTHING);
    let together = [
        tone(1, 5..6),
        tone(2, 6..7),
        tone(3, 7..8),
        tone(4, 8..9),
        tone(5, 9..10),
    ];
    assert_eq!(machine.due(ATT, since + ms(200)), together);
    assert_eq!(machine.stall, None);
    assert_eq!(machine.wait(ATT, since + ms(200)), Some(ms(20)));
    assert_eq!(machine.due(ATT, since + ms(220)), [tone(6, 10..11)]);
    assert_eq!(machine.largest_gap, Some(ms(100)));
}

#[test]
fn a_stall_longer_than_the_microphone_keeps_loses_no_frame() {
    /// The frames heard under the stall, twenty more than the microphone keeps.
    const STALLED: u16 = 100;
    assert!(usize::from(STALLED) > KEPT_FRAMES);
    let (mut machine, since) = armed();
    machine.burst = (1, 1);
    assert_eq!(start(&mut machine, since + ms(100)).len(), 2);
    let stall = FRAME * u32::from(STALLED);
    machine.stall = Some(stall);
    // The burst sent the frame at place 4 as packet 0, so the stall holds place 5 on.
    let together: Vec<Vec<u8>> = (0..STALLED)
        .map(|held| tone(u8::try_from(held + 1).unwrap(), held + 5..held + 6))
        .collect();
    // A channel with no stream asks first: it is owed nothing, and its asking takes nothing from the stream.
    assert_eq!(machine.due(OTHER_ATT, since + ms(100) + stall), NOTHING);
    assert_eq!(machine.due(ATT, since + ms(100) + stall), together);
    assert_eq!(
        machine.due(ATT, since + ms(120) + stall),
        [tone(101, 105..106)]
    );
    // Once sent, the frames of the stall are history, of which the microphone keeps its own count.
    assert_eq!(machine.kept_from(0).len(), KEPT_FRAMES);
}

#[test]
fn a_stall_with_no_audio_notification_before_it_holds_nothing_and_is_spent() {
    let (mut machine, since) = armed();
    machine.burst = (0, 5);
    assert_eq!(
        start(&mut machine, since + ms(105)),
        [sample::score(&TABLE)]
    );
    machine.stall = Some(ms(100));
    assert_eq!(machine.wait(ATT, since + ms(105)), Some(ms(15)));
    assert_eq!(machine.due(ATT, since + ms(120)), [tone(0, 5..6)]);
    assert_eq!(machine.stall, None);
    assert_eq!(machine.due(ATT, since + ms(140)), [tone(1, 6..7)]);
}

#[test]
fn a_stop_is_answered_with_the_end_and_its_reason_is_kept() {
    let (mut machine, since) = armed();
    machine.burst = (0, 5);
    // A STOP with no stream running is kept and answered with nothing.
    let unasked = host::stop(&TABLE, REASON_UNASKED);
    assert_eq!(machine.on_att(ATT, &unasked, since), NOTHING);
    assert_eq!(start(&mut machine, since + ms(100)).len(), 1);
    // A STOP on another channel is kept and ends nothing.
    subscribe(&mut machine, OTHER_ATT, &TABLE, since);
    let stop = host::stop(&TABLE, REASON_HOST);
    assert_eq!(machine.on_att(OTHER_ATT, &stop, since + ms(110)), NOTHING);
    assert!(machine.run.is_some());
    assert_eq!(
        machine.on_att(ATT, &stop, since + ms(110)),
        [sample::end(&TABLE)]
    );
    assert!(machine.run.is_none());
    assert_eq!(machine.due(ATT, since + ms(200)), NOTHING);
    assert_eq!(machine.stops, [REASON_UNASKED, REASON_HOST, REASON_HOST]);
}

#[test]
fn the_pods_end_a_stream_and_send_audio_nobody_asked_for() {
    let (mut machine, since) = armed();
    machine.burst = (0, 5);
    // With no stream there is nothing to end.
    machine.end_unasked();
    assert_eq!(machine.wait(ATT, since), None);
    assert_eq!(start(&mut machine, since + ms(100)).len(), 1);
    machine.end_unasked();
    assert_eq!(machine.wait(ATT, since + ms(100)), Some(Duration::ZERO));
    assert_eq!(machine.due(ATT, since + ms(200)), [sample::end(&TABLE)]);
    assert!(machine.run.is_none() && machine.stops.is_empty());

    machine.audio_unasked(2, since + ms(300));
    let unasked = [tone(0, 13..14), tone(1, 14..15)];
    assert_eq!(machine.due(ATT, since + ms(300)), unasked);
    assert!(machine.run.is_none());
    // More than the microphone has heard is all it has heard.
    let (mut machine, since) = armed();
    machine.audio_unasked(5, since + ms(40));
    assert_eq!(
        machine.due(ATT, since + ms(40)),
        [tone(0, 0..1), tone(1, 1..2)]
    );
}

#[test]
fn the_pods_own_request_and_indication_go_out_and_the_answers_are_counted() {
    let (mut machine, now) = armed();
    machine.owe_everywhere(&sample::request(MTU_REQUEST));
    machine.owe_everywhere(&sample::UNASKED_INDICATION);
    let unasked = [
        sample::request(MTU_REQUEST).to_vec(),
        sample::UNASKED_INDICATION.to_vec(),
    ];
    assert_eq!(machine.due(ATT, now), unasked);
    assert_eq!(machine.due(ATT, now), NOTHING);
    assert_eq!(
        machine.on_att(ATT, &host::refusal(MTU_REQUEST), now),
        NOTHING
    );
    assert_eq!(machine.on_att(ATT, &host::CONFIRMED, now), NOTHING);
    assert_eq!(machine.on_att(ATT, &host::CONFIRMED, now), NOTHING);
    assert_eq!((machine.refused, machine.confirmed), (1, 2));
}

#[test]
fn nothing_is_notified_on_a_channel_that_did_not_subscribe() {
    let mut machine = Machine::new();
    let since = Instant::now();
    machine.opened(ATT);
    assert_eq!(start(&mut machine, since), NOTHING);
    machine.on_control(CONTROL, &setting(DOAP_MODE, 1), since);
    assert_eq!(start(&mut machine, since + ms(100)), NOTHING);
    assert!(machine.run.is_some());
    assert_eq!(machine.due(ATT, since + ms(200)), NOTHING);
    // Audio that was not sent has no gap.
    assert_eq!(machine.largest_gap, None);
    machine.audio_unasked(1, since + ms(200));
    assert_eq!(machine.due(ATT, since + ms(200)), NOTHING);
    let stop = host::stop(&TABLE, REASON_HOST);
    assert_eq!(machine.on_att(ATT, &stop, since + ms(200)), NOTHING);
    assert_eq!((machine.starts, machine.stops.len()), (2, 1));
}

#[test]
fn a_descriptor_written_again_is_kept_once() {
    let (mut machine, now) = armed();
    for descriptor in TABLE.cccds {
        let written = machine.on_att(ATT, &host::subscribe(descriptor), now);
        assert_eq!(written, [sample::WRITTEN]);
    }
    assert_eq!(machine.lines, [(ATT, TABLE.cccds.to_vec())]);
}

#[test]
fn a_channel_that_ends_takes_what_ran_on_it() {
    let (mut machine, since) = armed();
    machine.burst = (0, 5);
    assert_eq!(start(&mut machine, since + ms(100)).len(), 1);
    machine.owe_everywhere(&sample::UNASKED_INDICATION);
    machine.closed(ATT);
    assert!(machine.run.is_none() && machine.mic.is_some());
    assert_eq!(machine.wait(ATT, since + ms(100)), None);
    assert_eq!(machine.due(ATT, since + ms(200)), NOTHING);

    // The control channel takes the mode with it, and the stream and a waiting START with the mode.
    for hold in [false, true] {
        let (mut machine, since) = armed();
        let settled = since + MODE_SETTLE;
        machine.hold_start = hold;
        machine.burst.0 = 0;
        assert_eq!(start(&mut machine, settled).len(), usize::from(!hold));
        machine.closed(OTHER_CONTROL);
        assert!(machine.mic.is_some());
        machine.closed(CONTROL);
        assert!(machine.mic.is_none() && machine.run.is_none() && machine.held.is_none());
        machine.release_start(settled);
        let refusal = sample::stopped(&TABLE, REASON_UNASKED);
        assert_eq!(start(&mut machine, settled), [refusal]);
    }
    // A START that waits goes with its own channel too.
    let (mut machine, since) = armed();
    let settled = since + MODE_SETTLE;
    machine.hold_start = true;
    assert_eq!(start(&mut machine, settled), NOTHING);
    machine.closed(ATT);
    assert!(machine.held.is_none());
    machine.release_start(settled);
    assert!(machine.run.is_none());
}
