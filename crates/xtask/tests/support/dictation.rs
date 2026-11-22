//! What the tests of the dictation audio share: the frame of the decoded file, what measures it whole and the share of it near zero.

/// The samples of one frame at 48 kHz, and the bytes of one sample.
pub(crate) const FRAME: usize = 960;
pub(crate) const SAMPLE_BYTES: usize = 4;
/// One step of a 16 bit sample.
const LSB_16: f32 = 1.0 / 32_768.0;
/// What `probe audio` is handed to measure the dictation audio whole, session by session.
pub(crate) const DICTATION: [&str; 3] = ["--pcm", "dictation.f32.wav", "--whole"];

/// The share of `samples` within one 16 bit step of zero, in percent, by the plainest arithmetic there is.
pub(crate) fn near_zero_percent(samples: &[f32]) -> f64 {
    let near = samples.iter().filter(|sample| sample.abs() <= LSB_16);
    let (near, all) = (near.count(), samples.len());
    f64::from(u32::try_from(near).unwrap()) * 100.0 / f64::from(u32::try_from(all).unwrap())
}
