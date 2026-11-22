//! A unit FFmpeg refuses and what it says of it, shared by the unit tests of the oracle and the test of the built command.

/// A unit FFmpeg refuses: it declares 63 bands, and a 480 sample frame at this rate has 35.
pub(crate) const REFUSED_UNIT: [u8; 2] = [0xff, 0xff];
/// What FFmpeg says of `REFUSED_UNIT`, from `decode_ics_info` in its libavcodec/aac/aacdec.c.
pub(crate) const REFUSED_MESSAGE: &str =
    "Number of scalefactor bands in group (63) exceeds limit (35).";
