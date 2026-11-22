//! The acoustic measures on generated signals: each finds what was planted and refuses what holds nothing.

use super::super::wav::Wav;
use super::alignment::{align, head};
use super::levels::{
    NoLevel, active, first_audible, floor_dbfs, is_digital_silence, level, near_zero_share,
    quietest, speech,
};
use super::onsets::{SameTaps, ratio_ppm, same_taps, taps};
use super::reference::Reference;
use super::signal::{
    BIN_HZ, OTHER_SEED, ROOM, SEED, add, band_noise, bursts, close, noise, rms_dbfs, samples_in,
    sine, span,
};
use super::spectrum::{NoCutoff, NoReach, cutoff, grid, reach};

/// The rate the stream is thought to run at.
const RATE: u32 = 64_000;
/// The rate of a reference recorder.
const REFERENCE_RATE: u32 = 48_000;
const TEN_SECONDS_OF_SLOTS: usize = 500;

fn fade_in(signal: &mut [f32], step: f32) {
    let mut gain = 0.0_f32;
    for sample in signal {
        *sample *= gain.min(1.0);
        gain += step;
    }
}

/// A click: one millisecond of loud noise.
fn click(seed: &mut u64, rate: u32) -> Vec<f32> {
    noise(seed, 0.5, samples_in(1, rate))
}

fn room_with_clicks(seed: &mut u64, rate: u32, ms: u32, at_ms: &[u32]) -> Vec<f32> {
    let mut room = noise(seed, ROOM, samples_in(ms, rate));
    for &at in at_ms {
        add(&mut room, samples_in(at, rate), &click(seed, rate));
    }
    room
}

#[test]
fn rms_and_peak_of_a_known_sine() {
    let found = level(&sine(1000.0, RATE, 0.5, 0.0, samples_in(1000, RATE))).unwrap();
    assert!(close(found.rms_dbfs, -9.031, 0.01), "{found:?}");
    assert!(close(found.peak_dbfs, -6.021, 0.01), "{found:?}");
    assert_eq!(level(&[]), Err(NoLevel::NoSample));
}

#[test]
fn quietest_window_finds_the_planted_silence() {
    let mut seed = SEED;
    let mut room = noise(&mut seed, 0.1, samples_in(4000, RATE));
    room.extend(noise(&mut seed, 0.001, samples_in(3000, RATE)));
    room.extend(noise(&mut seed, 0.1, samples_in(3000, RATE)));
    let found = quietest(&room, RATE, 3000).unwrap();
    let hop = samples_in(10, RATE);
    assert!(
        found.start.abs_diff(samples_in(4000, RATE)) <= hop,
        "{found:?}"
    );
    assert_eq!(found.len(), samples_in(3000, RATE));
    assert!(close(
        floor_dbfs(&room, RATE).unwrap(),
        rms_dbfs(&room[found]),
        0.01
    ));
    let short = &room[..samples_in(2990, RATE)];
    assert_eq!(quietest(short, RATE, 3000), None);
    assert_eq!(floor_dbfs(short, RATE), Err(NoLevel::NoFloor));
}

#[test]
fn digital_silence_is_reported_as_zeros() {
    let mut window = vec![0.0_f32; samples_in(4000, RATE)];
    assert!(is_digital_silence(&window));
    assert_eq!(level(&window), Err(NoLevel::DigitalSilence));
    assert_eq!(floor_dbfs(&window, RATE), Err(NoLevel::FloorOfZeros));
    assert_eq!(near_zero_share(&window), Some(1.0));
    // One sample of two 16 bit steps ends the silence and is outside the share.
    window[7] = 2.0 / 32768.0;
    assert!(!is_digital_silence(&window));
    let all_but_one = near_zero_share(&window).unwrap();
    assert!(all_but_one < 1.0 && all_but_one > 0.999, "{all_but_one}");
    assert_eq!(near_zero_share(&[]), None);
}

#[test]
fn grid_finds_a_brick_wall_at_16_khz_within_one_bin() {
    let mut seed = SEED;
    let walled = band_noise(&mut seed, 16_000.0, RATE, 0.01, samples_in(500, RATE));
    let found = cutoff(&grid(&[&walled], RATE)).unwrap();
    assert!(found.abs_diff(16_000) <= BIN_HZ, "{found}");
    // The same samples labelled 48 kHz hold three quarters of every frequency.
    let relabelled = cutoff(&grid(&[&walled], REFERENCE_RATE)).unwrap();
    assert!(relabelled.abs_diff(12_000) <= BIN_HZ, "{relabelled}");

    let room = noise(&mut seed, ROOM, samples_in(500, RATE));
    let mut jingle = band_noise(&mut seed, 10_000.0, RATE, 0.01, samples_in(500, RATE));
    add(&mut jingle, 0, &room);
    let reached = reach(&grid(&[&jingle], RATE), &grid(&[&room], RATE)).unwrap();
    assert!(reached.abs_diff(10_000) <= BIN_HZ, "{reached}");
    let same = reach(&grid(&[&room], RATE), &grid(&[&room], RATE));
    assert_eq!(same, Err(NoReach::Nowhere));
    // Over digital silence every point would lie, so there is no reach to read.
    let zeros = grid(&[&[0.0; 4096]], RATE);
    let over_zeros = reach(&grid(&[&jingle[..4096]], RATE), &zeros);
    assert_eq!(over_zeros, Err(NoReach::NothingUnder { hz: BIN_HZ }));
    assert_eq!(reach(&[], &[]), Err(NoReach::NoBlock));

    let white = cutoff(&grid(&[&room], RATE));
    assert_eq!(white, Err(NoCutoff::ReachesTheTop));
    assert_eq!(cutoff(&zeros), Err(NoCutoff::NoReference));
    assert_eq!(grid(&[&walled[..samples_in(10, RATE)]], RATE), []);
    assert_eq!(cutoff(&[]), Err(NoCutoff::NoBlock));
}

#[test]
fn taps_are_found_within_2_ms_and_speech_is_not_a_tap() {
    let mut seed = SEED;
    // The click 60 ms after the first is the same tap bouncing.
    let mut room = room_with_clicks(&mut seed, RATE, 3000, &[1000, 1060, 1250]);
    let len = samples_in(500, RATE);
    // A vowel: its partials above 2 kHz lie 32 dB under its fundamental, and it comes up over 30 ms.
    let mut voice = sine(200.0, RATE, 0.2, 0.0, len);
    add(&mut voice, 0, &sine(2400.0, RATE, 0.005, 0.0, len));
    add(&mut voice, 0, &sine(3200.0, RATE, 0.005, 0.0, len));
    fade_in(&mut voice, 1.0 / 1920.0);
    add(&mut room, samples_in(2000, RATE), &voice);

    let found = taps(&room, RATE);
    assert_eq!(found.len(), 2, "{found:?}");
    for (onset, planted_ms) in found.iter().zip([1000, 1250]) {
        let apart = onset.abs_diff(samples_in(planted_ms, RATE));
        assert!(apart <= samples_in(2, RATE), "{found:?}");
    }
    assert_eq!(
        taps(&room[..samples_in(900, RATE)], RATE),
        Vec::<usize>::new()
    );
}

#[test]
fn first_audible_frame_of_a_ramp() {
    let mut ramp = sine(1000.0, RATE, 0.1, 0.0, samples_in(1000, RATE));
    fade_in(&mut ramp, 1.0 / 64_000.0);
    // The ramp passes -60 dBFS 14 ms in, so the first 20 ms frame stays under it and the second does not.
    assert_eq!(first_audible(&ramp, RATE), Some(samples_in(20, RATE)));
    assert_eq!(first_audible(&vec![0.0; 6400], RATE), None);
    let mut seed = SEED;
    assert_eq!(first_audible(&noise(&mut seed, ROOM, 64_000), RATE), None);
}

#[test]
fn ratio_of_a_33_percent_stretch_is_reported() {
    let reference = [3.0, 3.25, 79.0, 79.3];
    // Samples of a 64 kHz clock counted as 48 kHz last a third longer.
    let stretched = reference.map(|at_s| at_s * 64.0 / 48.0);
    let found = ratio_ppm(&stretched, &reference).unwrap();
    assert!(close(found, 333_333.3, 1.0), "{found}");
    assert_eq!(ratio_ppm(&reference[..3], &reference), None);
    assert_eq!(ratio_ppm(&[3.0], &[3.0]), None);
    assert_eq!(ratio_ppm(&[1.0, 2.0], &[5.0, 5.0]), None);
}

#[test]
fn taps_are_the_same_only_where_each_lies_on_its_own() {
    let reference = [3.0, 3.25, 79.0, 79.3];
    // A stretch moves no tap off its own.
    let stretched = reference.map(|at_s| at_s * 64.0 / 48.0);
    assert_eq!(same_taps(&stretched, &reference), SameTaps::Shown);
    // The first tap is of another event, and the first tap found off its own is the second.
    let another_first = [0.4, 2.0, 3.0, 4.0];
    let found = same_taps(&another_first, &[1.0, 2.0, 3.0, 4.0]);
    assert_eq!(found, SameTaps::Not { tap: 2 });
    // A tap is off its own from 50 ms on: laid on a tap at zero, 0.05 s is the bound itself.
    let around_zero = [-1.0, 0.0, 1.0];
    assert_eq!(
        same_taps(&[-1.0, 0.05, 1.0], &around_zero),
        SameTaps::Not { tap: 2 }
    );
    assert_eq!(
        same_taps(&[-1.0, 0.049, 1.0], &around_zero),
        SameTaps::Shown
    );
    // Two taps leave none between, and one or none leaves nothing to lay.
    assert_eq!(same_taps(&[0.4, 4.0], &[1.0, 4.0]), SameTaps::NotShown);
    assert_eq!(same_taps(&[0.4], &[1.0]), SameTaps::NotShown);
    assert_eq!(same_taps(&[], &[]), SameTaps::NotShown);
}

#[test]
fn ratio_of_equal_clocks_is_under_20_ppm() {
    let onsets_s = |rate: u32, seed: u64| -> Vec<f64> {
        let mut seed = seed;
        let room = room_with_clicks(&mut seed, rate, 21_000, &[500, 20_500]);
        let found = taps(&room, rate);
        assert_eq!(found.len(), 2, "{found:?}");
        let seconds = |&onset: &usize| f64::from(u32::try_from(onset).unwrap()) / f64::from(rate);
        found.iter().map(seconds).collect()
    };
    let found = ratio_ppm(&onsets_s(RATE, SEED), &onsets_s(REFERENCE_RATE, OTHER_SEED)).unwrap();
    assert!(found.abs() < 20.0, "{found}");
}

#[test]
fn align_finds_a_planted_offset_within_5_ms() {
    let reference = bursts(SEED, 1, REFERENCE_RATE, TEN_SECONDS_OF_SLOTS);
    let scene = bursts(SEED, 2, RATE, TEN_SECONDS_OF_SLOTS);
    // The stream is quieter than the reference, has another carrier and a room of its own.
    let mut seed = SEED;
    let mut stream: Vec<f32> = scene[span(2345, 3000, RATE)].to_vec();
    for sample in &mut stream {
        *sample *= 0.3;
    }
    let room = noise(&mut seed, ROOM, stream.len());
    add(&mut stream, 0, &room);

    let found = align(&stream, RATE, &reference, REFERENCE_RATE, 2000, 1000).unwrap();
    assert!(found.offset_ms.abs_diff(2345) <= 5, "{found:?}");
    assert!(found.score > 0.9, "{found:?}");
    // The planted offset is outside what is searched: nothing is found there.
    let elsewhere = align(&stream, RATE, &reference, REFERENCE_RATE, 6000, 1000);
    assert_eq!(elsewhere, None);
}

#[test]
fn align_refuses_uncorrelated_signals() {
    let reference = bursts(SEED, 1, REFERENCE_RATE, TEN_SECONDS_OF_SLOTS);
    let other = bursts(OTHER_SEED, 2, RATE, TEN_SECONDS_OF_SLOTS);
    let stream = &other[..samples_in(3000, RATE)];
    assert_eq!(
        align(stream, RATE, &reference, REFERENCE_RATE, 2000, 1000),
        None
    );
    let silent = vec![0.0; stream.len()];
    assert_eq!(
        align(&silent, RATE, &reference, REFERENCE_RATE, 2000, 1000),
        None
    );
    // A search that leaves the reference on both sides finds nothing to compare.
    assert_eq!(
        align(
            &reference,
            REFERENCE_RATE,
            &reference[..4800],
            REFERENCE_RATE,
            0,
            1000
        ),
        None
    );
}

/// Four seconds of the scene from `from_ms` on, with the first second taken `head_late_ms` later than it should be.
fn start_segment(scene: &[f32], from_ms: u32, head_late_ms: u32) -> Vec<f32> {
    let mut segment = scene[span(from_ms.saturating_add(head_late_ms), 1000, RATE)].to_vec();
    segment.extend(&scene[span(from_ms.saturating_add(1000), 3000, RATE)]);
    segment
}

#[test]
fn head_and_body_must_align_at_one_offset() {
    let reference = bursts(SEED, 1, REFERENCE_RATE, TEN_SECONDS_OF_SLOTS);
    let scene = bursts(SEED, 2, RATE, TEN_SECONDS_OF_SLOTS);

    let whole = head(
        &start_segment(&scene, 3000, 0),
        RATE,
        &reference,
        REFERENCE_RATE,
        3200,
    );
    let (first, rest) = (whole.head.unwrap(), whole.body.unwrap());
    assert!(
        first.offset_ms.abs_diff(3000) <= 5 && rest.offset_ms.abs_diff(3000) <= 5,
        "{whole:?}"
    );
    assert!(first.agrees_with(rest), "{whole:?}");

    for late_ms in [400, 40] {
        let spliced = head(
            &start_segment(&scene, 3000, late_ms),
            RATE,
            &reference,
            REFERENCE_RATE,
            3200,
        );
        let planted = i64::from(late_ms).saturating_add(3000);
        let (first, rest) = (spliced.head.unwrap(), spliced.body.unwrap());
        assert!(first.offset_ms.abs_diff(planted) <= 5, "{spliced:?}");
        assert!(rest.offset_ms.abs_diff(3000) <= 5, "{spliced:?}");
        assert!(!first.agrees_with(rest), "{late_ms} ms: {spliced:?}");
    }
}

#[test]
fn a_silent_head_is_reported_not_aligned() {
    let reference = bursts(SEED, 1, REFERENCE_RATE, TEN_SECONDS_OF_SLOTS);
    let scene = bursts(SEED, 2, RATE, TEN_SECONDS_OF_SLOTS);
    let mut segment = start_segment(&scene, 3000, 0);
    let sounding = head(&segment, RATE, &reference, REFERENCE_RATE, 3000);
    let second = samples_in(1000, RATE);
    let planted = rms_dbfs(&segment[..second]) - rms_dbfs(&segment[second..]);
    assert!(
        close(sounding.head_minus_body_db.unwrap(), planted, 0.01),
        "{sounding:?}"
    );

    segment[..second].fill(0.0);
    let muted = head(&segment, RATE, &reference, REFERENCE_RATE, 3000);
    assert_eq!(muted.head, None);
    assert!(muted.body.is_some(), "{muted:?}");
    assert_eq!(muted.head_minus_body_db, None);
}

#[test]
fn a_gated_signal_is_told_from_an_ungated_one() {
    let mut seed = SEED;
    let silent = noise(&mut seed, ROOM, samples_in(4000, RATE));
    let talk = |seed: &mut u64, ms| noise(seed, 0.2, samples_in(ms, RATE));
    // Ungated, the breath between two sentences stays 20 dB over the room; gated, it falls to the room.
    let between = |seed: &mut u64, amplitude| noise(seed, amplitude, samples_in(3200, RATE));
    let window =
        |seed: &mut u64, pause: Vec<f32>| [talk(seed, 1500), pause, talk(seed, 1500)].concat();
    let breath = between(&mut seed, ROOM * 10.0);
    let ungated = window(&mut seed, breath);
    let room = between(&mut seed, ROOM);
    let gated = window(&mut seed, room);

    let over_silent =
        |speech: &[f32]| floor_dbfs(speech, RATE).unwrap() - floor_dbfs(&silent, RATE).unwrap();
    assert!(
        close(over_silent(&ungated), 20.0, 0.5),
        "{}",
        over_silent(&ungated)
    );
    assert!(
        close(over_silent(&gated), 0.0, 0.5),
        "{}",
        over_silent(&gated)
    );
}

#[test]
fn a_window_with_no_active_frame_is_refused_not_zero() {
    let muted = vec![0.0_f32; samples_in(2000, RATE)];
    assert_eq!(active(&muted, RATE), []);
    assert_eq!(speech(&muted, RATE), Err(NoLevel::DigitalSilence));
    assert_eq!(
        speech(&muted[..samples_in(19, RATE)], RATE),
        Err(NoLevel::NoFrame)
    );
    // A room nobody speaks in has frames within 10 dB of its loudest, and none of them is audible.
    let mut seed = SEED;
    let room = noise(&mut seed, ROOM, samples_in(2000, RATE));
    assert_ne!(active(&room, RATE), Vec::new());
    assert_eq!(speech(&room, RATE), Err(NoLevel::NotAudible));
    // Zeros that end in a sound shorter than a frame are not digital silence, and hold no audible frame.
    let mut tail = muted.clone();
    tail.extend(noise(&mut seed, 0.5, samples_in(5, RATE)));
    assert_eq!(speech(&tail, RATE), Err(NoLevel::NotAudible));

    // A second of sine in three seconds of room: only the sine is active, and its level is the sine's own.
    let mut window = noise(&mut seed, ROOM, samples_in(3000, RATE));
    add(
        &mut window,
        samples_in(1000, RATE),
        &sine(1000.0, RATE, 0.25, 0.0, samples_in(1000, RATE)),
    );
    let sounding = samples_in(1000, RATE)..samples_in(2000, RATE);
    assert_eq!(active(&window, RATE), vec![sounding; 1]);
    let (found, frames) = speech(&window, RATE).unwrap();
    assert_eq!(frames, 50);
    assert!(close(found.rms_dbfs, -15.05, 0.02), "{found:?}");
    assert!(close(found.peak_dbfs, -12.04, 0.02), "{found:?}");
}

#[test]
fn a_stamp_is_placed_in_the_reference_to_the_millisecond_below_it() {
    let wav = Wav {
        rate: REFERENCE_RATE,
        channels: 1,
        samples: vec![0.0; samples_in(3000, REFERENCE_RATE)],
    };
    // The first sample of the reference lies 2 ms after the zero of the run.
    let reference = Reference::new(&wav, 2_000_000, 0);
    assert_eq!(reference.ms_at(2_500_000), Some(0));
    assert_eq!(reference.ms_at(1_500_000), Some(-1));
    assert_eq!(reference.ms_at(1_000_000), Some(-1));
    assert_eq!(reference.stamp_ns(48), Some(3_000_000));
    assert!(reference.holds(0, 3000) && !reference.holds(-1, 3000) && !reference.holds(0, 3001));

    let other = Wav {
        rate: 44_100,
        channels: 1,
        samples: vec![0.0; 44_100],
    };
    // The recorder started 5 ms after the zero of a run whose zero is not the zero of the wall clock.
    let later = Reference::new(&other, 1_000_005_000_000, 1_000_000_000_000);
    assert_eq!(later.ms_at(5_000_000), Some(0));
    assert_eq!(later.ms_at(4_999_999), Some(-1));
    assert_eq!(later.stamp_ns(441), Some(15_000_000));
    assert!(later.holds(0, 1000) && !later.holds(0, 1001));
}
