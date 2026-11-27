use std::ffi::c_void;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
use std::sync::mpsc;

use super::{Event, Phase, STRIDE, Steering, Watch, ffi, frames};

/// The frames of the buffer the tests hand over, and its bytes.
const HELD: u32 = 8;
const HELD_BYTES: u32 = HELD * STRIDE;
const RATIO: f64 = 1.001;

/// What `frames` gives for a buffer of `n_datas` blocks, the first `maxsize` bytes at `data` with a chunk or none: its slice length, count and capacity.
#[expect(unsafe_code, reason = "runs the reader over a buffer made here")]
fn read(
    data: *mut c_void,
    maxsize: u32,
    n_datas: u32,
    with_chunk: bool,
    requested: u64,
) -> Option<(usize, u32, usize)> {
    let mut chunk = ffi::spa_chunk {
        offset: 0,
        size: 0,
        stride: 0,
        flags: 0,
    };
    let chunk = if with_chunk {
        &raw mut chunk
    } else {
        ptr::null_mut()
    };
    let mut block = ffi::spa_data {
        type_: 0,
        flags: 0,
        fd: -1,
        mapoffset: 0,
        maxsize,
        data,
        chunk,
    };
    let mut spa = ffi::spa_buffer {
        n_metas: 0,
        n_datas,
        metas: ptr::null_mut(),
        datas: &raw mut block,
    };
    let buffer = buffer(&raw mut spa, requested);
    // SAFETY: every pointer is null or to a local that outlives the call, with `maxsize` bytes behind `data`.
    unsafe { frames(&buffer) }.map(|frames| (frames.samples.len(), frames.count, frames.capacity))
}

fn buffer(spa: *mut ffi::spa_buffer, requested: u64) -> ffi::pw_buffer {
    ffi::pw_buffer {
        buffer: spa,
        user_data: ptr::null_mut(),
        size: 0,
        requested,
        time: 0,
    }
}

#[test]
fn a_request_gives_its_frames_capped_at_the_buffer_and_zero_gives_it_whole() {
    let mut samples = vec![0.0_f32; usize::try_from(HELD).unwrap()];
    let data = samples.as_mut_ptr().cast();
    for (requested, count) in [(0, HELD), (3, 3), (9, HELD), (u64::MAX, HELD)] {
        let given = read(data, HELD_BYTES, 1, true, requested);
        let wanted = (usize::try_from(count).unwrap(), count, samples.len());
        assert_eq!(given, Some(wanted), "{requested} requested");
    }
}

#[test]
fn a_buffer_that_is_not_whole_mapped_frames_is_left_alone() {
    let mut samples = vec![0.0_f32; usize::try_from(HELD).unwrap()];
    let data: *mut c_void = samples.as_mut_ptr().cast();
    let odd = data.wrapping_byte_add(1);
    let cases = [
        ("no whole frame", data, HELD_BYTES - 1, 1, true),
        ("unmapped", ptr::null_mut(), HELD_BYTES, 1, true),
        ("misaligned", odd, HELD_BYTES - STRIDE, 1, true),
        ("no block", data, HELD_BYTES, 0, true),
        ("no chunk", data, HELD_BYTES, 1, false),
    ];
    for (name, data, maxsize, n_datas, with_chunk) in cases {
        assert_eq!(read(data, maxsize, n_datas, with_chunk, 0), None, "{name}");
    }
}

#[test]
#[expect(unsafe_code, reason = "runs the reader over a buffer with no blocks")]
fn a_null_buffer_is_left_alone() {
    let buffer = buffer(ptr::null_mut(), 0);
    // SAFETY: the one pointer is null.
    assert!(unsafe { frames(&buffer) }.is_none());
}

#[test]
fn a_stream_is_lost_once_and_reports_nothing_after() {
    let (seen, events) = mpsc::channel();
    let mut watch = Watch {
        phase: Phase::Open,
        events: Box::new(move |event| seen.send(event).unwrap_or(())),
    };
    for state in [
        ffi::PW_STREAM_STATE_STREAMING,
        ffi::PW_STREAM_STATE_PAUSED,
        ffi::PW_STREAM_STATE_ERROR,
        ffi::PW_STREAM_STATE_UNCONNECTED,
        ffi::PW_STREAM_STATE_STREAMING,
        ffi::PW_STREAM_STATE_PAUSED,
    ] {
        watch.changed(state, String::new());
    }
    let reported: Vec<Event> = events.try_iter().collect();
    assert_eq!(reported, [Event::Streaming, Event::Paused, Event::Lost]);
}

/// A steering with no area yet, an area as the converter starts it, and the size the server gives with it.
fn steering() -> (Steering, ffi::spa_io_rate_match, u32) {
    let steering = Steering {
        area: AtomicPtr::new(ptr::null_mut()),
        unsteered: Arc::new(AtomicU64::new(0)),
    };
    let area = ffi::spa_io_rate_match {
        delay: 0,
        size: 0,
        rate: 1.0,
        flags: 0,
        padding: [0; 7],
    };
    let size = u32::try_from(size_of::<ffi::spa_io_rate_match>()).unwrap();
    (steering, area, size)
}

#[test]
#[expect(unsafe_code, reason = "hands the steering an area made here")]
fn a_ratio_reaches_the_area_as_its_inverse_with_the_flag() {
    let (steering, mut area, size) = steering();
    // SAFETY: the area outlives the steering's use of it.
    unsafe { steering.set((&raw mut area).cast(), size) };
    steering.steer(RATIO);
    assert!(
        (area.rate - 1.0 / RATIO).abs() < f64::EPSILON,
        "{}",
        area.rate
    );
    assert_eq!(area.flags, ffi::SPA_IO_RATE_MATCH_FLAG_ACTIVE);
    assert_eq!(steering.unsteered.load(Ordering::Relaxed), 0);
}

#[test]
#[expect(unsafe_code, reason = "hands the steering an area made here")]
fn with_no_area_nothing_is_written_and_the_cycle_is_counted() {
    let (steering, mut area, size) = steering();
    steering.steer(RATIO);
    // SAFETY: the area outlives the steering's use of it; one byte short, it is not kept.
    unsafe { steering.set((&raw mut area).cast(), size.saturating_sub(1)) };
    steering.steer(RATIO);
    // SAFETY: as above, whole this time.
    unsafe { steering.set((&raw mut area).cast(), size) };
    // SAFETY: null is the area taken back.
    unsafe { steering.set(ptr::null_mut(), size) };
    steering.steer(RATIO);
    assert!((area.rate - 1.0).abs() < f64::EPSILON, "{}", area.rate);
    assert_eq!(area.flags, 0);
    assert_eq!(steering.unsteered.load(Ordering::Relaxed), 3);
}
