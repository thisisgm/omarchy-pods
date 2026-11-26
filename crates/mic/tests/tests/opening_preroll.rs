use std::io::{self, Write};

use super::{
    BURST_FRAMES, COLLAR_FRAMES, FADE, FRAME_MS, QUANTUM, RATE_HZ, Rig, SHORT_LEAD_MS, STEADY,
    Take, after, assert_same, bytes, decoded, frame_frames, frames_in, head, iter, linked, ms,
    places, silence, steady, tone_onset,
};

fn assert_bounded_catch_up(take: &Take) {
    const MAX_RATIO: f64 = 1.045;
    let fast: Vec<_> = take
        .ratios
        .iter()
        .copied()
        .filter(|&ratio| !steady(ratio))
        .collect();
    assert!(!fast.is_empty(), "reader never requested catch-up");
    assert!(fast.len().saturating_mul(QUANTUM) <= usize::try_from(RATE_HZ).unwrap());
    assert!(fast.iter().all(|&ratio| ratio <= MAX_RATIO));
    assert_eq!(take.allocations, 0);
}

#[test]
fn a_late_opening_anchor_does_not_realign_past_the_first_word() {
    const SILENT: usize = 10;
    const READS: usize = 40;
    // 11 frames split 5, 5, 1: the trailing 1-frame burst chunk places 17 ms late (well inside DOAP_JITTER_COVER_MS), and the cadence catches up while the reader is inside the first word.
    const LATE_STEADY: usize = 3;
    const ANCHOR_LATE_MS: u32 = 17;
    let placed = STEADY.saturating_sub(ms(FRAME_MS - ANCHOR_LATE_MS));
    let mut burst_frames = vec![silence(); SILENT];
    burst_frames.push(tone_onset());
    let mut take = Take::new(0, None);
    take.play(
        Vec::new(),
        linked(STEADY, SHORT_LEAD_MS),
        head(SHORT_LEAD_MS) / QUANTUM,
    );
    for (index, chunk) in burst_frames.chunks(BURST_FRAMES).enumerate() {
        let offset = index.saturating_mul(BURST_FRAMES);
        let counters = (offset..).map(|counter| u16::try_from(counter).unwrap());
        take.chain
            .frames(placed, counters.zip(chunk.iter().copied()));
    }
    take.lands = (0..READS)
        .map(|index| {
            let on_cadence = after(STEADY, ms(FRAME_MS), u32::try_from(index).unwrap());
            let at = if index < LATE_STEADY {
                on_cadence + ms(ANCHOR_LATE_MS)
            } else {
                on_cadence
            };
            (at, vec![burst_frames.len().saturating_add(index)])
        })
        .collect();
    take.go_on(READS);

    let mut script = burst_frames;
    script.extend(iter::repeat_n(bytes(0, None), READS));
    let twin = decoded(&script);
    let kept_start = (SILENT - COLLAR_FRAMES) * frame_frames();
    let word = &twin[SILENT * frame_frames()..(SILENT + 3) * frame_frames()];
    let found = places(&take.tape, word);
    assert_eq!(
        found.len(),
        1,
        "the opening word must play intact without a forward realign jump"
    );
    let audio_start = found[0] - COLLAR_FRAMES * frame_frames();
    let expected_len = take.tape.len() - audio_start;
    assert_same(
        &take.tape[audio_start + FADE..],
        &twin[kept_start + FADE..kept_start + expected_len],
    );
    assert_bounded_catch_up(&take);
    assert_eq!(Rig::count(&take.counters.underruns), 0);
}

#[test]
fn a_stalled_reader_in_a_short_opening_catches_up_without_dropping_audio() {
    const SILENT: usize = 10;
    const STALL_MS: u32 = 120;
    // The 15 ms steady bound and 43 ms of rate recovery leave at most 62 ms to cut.
    const MAX_CUT_MS: u32 = 62;
    const READS: usize = 120;
    let mut burst_frames = vec![silence(); SILENT];
    burst_frames.push(tone_onset());
    let mut take = Take::new(0, None);
    take.play(
        Vec::new(),
        linked(STEADY, SHORT_LEAD_MS),
        head(SHORT_LEAD_MS) / QUANTUM,
    );
    for (index, chunk) in burst_frames.chunks(BURST_FRAMES).enumerate() {
        let offset = index.saturating_mul(BURST_FRAMES);
        let counters = (offset..).map(|counter| u16::try_from(counter).unwrap());
        take.chain
            .frames(STEADY, counters.zip(chunk.iter().copied()));
    }
    take.lands = (0..READS)
        .map(|index| {
            let at = after(
                STEADY + ms(FRAME_MS),
                ms(FRAME_MS),
                u32::try_from(index).unwrap(),
            );
            (at, vec![burst_frames.len().saturating_add(index)])
        })
        .collect();
    // Read once after placement so the short 80 ms opening has begun and exited, then stall the reader for 120 ms.
    take.go_on(1);
    let (before, _) = take.reader.read_ends();
    take.now = take.now.saturating_add(ms(STALL_MS));
    take.go_on(1);
    let (after, _) = take.reader.read_ends();
    let cut = after - before - i64::try_from(QUANTUM).unwrap();
    let max_cut = i64::try_from(frames_in(ms(MAX_CUT_MS))).unwrap();
    assert!((0..=max_cut).contains(&cut), "stall cut {cut} frames");
    writeln!(
        io::stderr(),
        "reader skipped {cut} silent frames after the stall"
    )
    .unwrap();
    take.go_on(READS - 2);

    let mut script = burst_frames;
    script.extend(iter::repeat_n(bytes(0, None), READS));
    let twin = decoded(&script);
    let kept_start = (SILENT - COLLAR_FRAMES) * frame_frames();
    let word = &twin[SILENT * frame_frames()..(SILENT + 3) * frame_frames()];
    let found = places(&take.tape, word);
    assert_eq!(
        found.len(),
        1,
        "a 120 ms reader stall must catch up smoothly instead of skipping the opening word"
    );
    let audio_start = found[0] - COLLAR_FRAMES * frame_frames();
    let expected_len = take.tape.len() - audio_start;
    assert_same(
        &take.tape[audio_start + FADE..],
        &twin[kept_start + FADE..kept_start + expected_len],
    );
    assert!(
        steady(*take.ratios.last().unwrap()),
        "reader must return to steady steering after catching up"
    );
    assert_bounded_catch_up(&take);
    assert_eq!(Rig::count(&take.counters.underruns), 0);
}
