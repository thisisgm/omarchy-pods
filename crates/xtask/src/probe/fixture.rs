//! Units and a check shared by the unit tests of the decoders and the test of the built command.

use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};

/// A unit that is not silence: a gain of 0xb4, one band coded with the first codebook, and in it one quad that holds one line.
pub(crate) const TONE_UNIT: [u8; 4] = [0xb4, 0x04, 0x42, 0x50];
/// A unit FFmpeg refuses: it declares 63 bands, and a 480 sample frame at this rate has 35.
pub(crate) const REFUSED_UNIT: [u8; 2] = [0xff, 0xff];
/// What FFmpeg says of `REFUSED_UNIT`, from `decode_ics_info` in its libavcodec/aac/aacdec.c.
pub(crate) const REFUSED_MESSAGE: &str =
    "Number of scalefactor bands in group (63) exceeds limit (35).";

/// Whether `tool` starts here; where it does not, the test that asked says so on its standard error and passes without it.
pub(crate) fn installed(tool: &str) -> bool {
    let mut probe = Command::new(tool);
    probe
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let found = probe.status().is_ok();
    if !found {
        writeln!(io::stderr(), "skipped: {tool} is not installed").unwrap();
    }
    found
}

/// What ffprobe reads from the first stream of `file`, one `key=value` a line, with every packet counted.
pub(crate) fn ffprobe(file: &Path, entries: &str) -> String {
    let probed = Command::new("ffprobe")
        .args(["-v", "error", "-count_packets", "-show_entries", entries])
        .args(["-of", "default=noprint_wrappers=1"])
        .arg(file)
        .output()
        .unwrap();
    assert!(probed.status.success(), "{probed:?}");
    assert_eq!(String::from_utf8(probed.stderr).unwrap(), "");
    String::from_utf8(probed.stdout).unwrap()
}
