//! The check every test that needs FFmpeg's Opus decoders makes first.

use std::io::{self, Write};
use std::process::{Command, Stdio};

/// Whether FFmpeg, its prober and both of its Opus decoders are here; where one is not, the test that asked says so on its standard error and passes without it.
pub(crate) fn opus_oracles_installed() -> bool {
    let starts = |tool: &str| {
        let mut probe = Command::new(tool);
        probe
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        probe.status().is_ok()
    };
    let listed = Command::new("ffmpeg")
        .args(["-hide_banner", "-decoders"])
        .stderr(Stdio::null())
        .output();
    let listed =
        listed.map_or_default(|listed| String::from_utf8_lossy(&listed.stdout).into_owned());
    // Sample input: ` A....D libopus              libopus Opus (codec opus)`, one line of the list of decoders.
    let lists = |decoder: &str| {
        let mut names = listed.lines().map(|line| line.split_whitespace().nth(1));
        names.any(|name| name == Some(decoder))
    };
    let found = starts("ffprobe") && lists("libopus") && lists("opus");
    if !found {
        let skipped = "skipped: ffmpeg, ffprobe or one of the two Opus decoders is not installed";
        writeln!(io::stderr(), "{skipped}").unwrap();
    }
    found
}
