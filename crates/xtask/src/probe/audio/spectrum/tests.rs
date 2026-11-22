//! Where a band ends, on spectra written by hand: the fall that is an edge, and each shape that has none.

use super::super::signal::TILT_FROM_HZ;
use super::{EDGE_MIN_BLOCKS, GRID_STEP_HZ, NoEdge, band_edge, block_len, edge_of, ratio};

/// A grid to 24 kHz whose point at `hz` holds the power `level_db(hz)` gives in dB.
fn spectrum(level_db: impl Fn(u32) -> f64) -> Vec<(u32, f64)> {
    let points = (1..=96_u32).map(|step| step.saturating_mul(GRID_STEP_HZ));
    points.map(|hz| (hz, ratio(level_db(hz)))).collect()
}

/// A voice: flat to where the generated voice starts to tilt, then `tilt_db` an octave down.
fn voice(hz: u32, tilt_db: f64) -> f64 {
    -tilt_db * (f64::from(hz) / f64::from(TILT_FROM_HZ)).max(1.0).log2()
}

#[test]
fn the_edge_is_the_wall_and_not_the_tilt_of_the_voice() {
    for tilt_db in [6.0, 12.0, 24.0] {
        let walled = |hz| voice(hz, tilt_db) - if hz > 8000 { 30.0 } else { 0.0 };
        assert_eq!(edge_of(&spectrum(walled)), Ok(8000), "{tilt_db}");
    }
}

#[test]
fn a_valley_the_spectrum_climbs_out_of_is_no_edge() {
    // Twenty dB down from 3 to 5 kHz, as between two formants, then a hump as high as before it.
    let valley = |hz| {
        voice(hz, 0.0)
            - if (3000..=5000).contains(&hz) {
                20.0
            } else {
                0.0
            }
    };
    let walled = |hz| valley(hz) - if hz > 8000 { 30.0 } else { 0.0 };
    assert_eq!(edge_of(&spectrum(walled)), Ok(8000));
    assert_eq!(edge_of(&spectrum(valley)), Err(NoEdge::NoFall));
}

#[test]
fn of_two_walls_the_edge_is_the_lower() {
    // A voice walled at 8 kHz over a floor that a resampler walls at 22 kHz, the deeper fall of the two.
    let twice = |hz| match hz {
        ..=8000 => voice(hz, 9.0),
        8001..=22_000 => -80.0,
        _ => -160.0,
    };
    assert_eq!(edge_of(&spectrum(twice)), Ok(8000));
}

#[test]
fn a_fall_under_1_khz_or_under_12_db_is_no_edge() {
    // The same fall of 30 dB one grid step under 1 kHz, where none is looked for, and from 1 kHz itself.
    let under = |hz| if hz >= 1000 { -30.0 } else { 0.0 };
    assert_eq!(edge_of(&spectrum(under)), Err(NoEdge::NoFall));
    let at = |hz| if hz > 1000 { -30.0 } else { 0.0 };
    assert_eq!(edge_of(&spectrum(at)), Ok(1000));
    let shallow = |hz| if hz > 8000 { -11.9 } else { 0.0 };
    assert_eq!(edge_of(&spectrum(shallow)), Err(NoEdge::NoFall));
    let enough = |hz| if hz > 8000 { -12.1 } else { 0.0 };
    assert_eq!(edge_of(&spectrum(enough)), Ok(8000));
    // A fade of 3 dB a step falls 60 dB from 8 to 13 kHz and has no edge anywhere.
    let fade = |hz: u32| -3.0 * f64::from(hz.saturating_sub(8000) / GRID_STEP_HZ);
    assert_eq!(edge_of(&spectrum(fade)), Err(NoEdge::NoFall));
}

#[test]
fn nothing_over_the_wall_is_an_edge_and_nothing_at_all_is_none() {
    let mut walled = spectrum(|hz| voice(hz, 9.0));
    for (_, power) in walled.iter_mut().filter(|(hz, _)| *hz > 8000) {
        *power = 0.0;
    }
    assert_eq!(edge_of(&walled), Ok(8000));
    let silent: Vec<(u32, f64)> = walled.iter().map(|&(hz, _)| (hz, 0.0)).collect();
    assert_eq!(edge_of(&silent), Err(NoEdge::Nothing));
    assert_eq!(edge_of(&[]), Err(NoEdge::Nothing));
    // A power that is no number is never an edge and hides none.
    let (at, _) = walled
        .iter()
        .enumerate()
        .find(|(_, point)| point.0 == 4000)
        .unwrap();
    walled[at].1 = f64::NAN;
    assert_eq!(edge_of(&walled), Ok(8000));
}

#[test]
fn a_stretch_of_too_few_blocks_is_refused_by_their_count() {
    let enough = block_len(48_000).saturating_mul(EDGE_MIN_BLOCKS);
    let short = vec![0.5; enough.saturating_sub(1)];
    let blocks = EDGE_MIN_BLOCKS.saturating_sub(1);
    assert_eq!(band_edge(&short, 48_000), Err(NoEdge::Thin { blocks }));
    let enough = vec![0.0; enough];
    assert_eq!(band_edge(&enough, 48_000), Err(NoEdge::Nothing));
}
