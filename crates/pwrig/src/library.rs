//! The `PipeWire` client library a process mapped, by the version in its file name.

use std::fs;
use std::path::Path;

/// The file name every version of the library starts with.
const SONAME: &str = "libpipewire-0.3.so.0";
/// The first library whose streams state a process latency: 1.3.81, as its file name counts it.
const STATES_LATENCY_SINCE: u32 = 1381;

/// Whether the library mapped in `maps`, a process's `/proc/<pid>/maps`, states a process latency.
#[must_use]
pub fn states_latency(maps: &Path) -> bool {
    let text =
        fs::read_to_string(maps).unwrap_or_else(|error| panic!("{}: {error}", maps.display()));
    version(&text) >= STATES_LATENCY_SINCE
}

/// The version in the mapped library's file name, 1402 for 1.4.2.
fn version(maps: &str) -> u32 {
    // Sample input: `7f3a1c000000-7f3a1c00f000 r--p 00000000 08:01 1234 /usr/lib/x86_64-linux-gnu/libpipewire-0.3.so.0.1402.0`.
    let file = maps
        .lines()
        .filter_map(|line| line.rsplit('/').next())
        .find(|file| file.starts_with(SONAME))
        .unwrap_or_else(|| panic!("no {SONAME} among the mapped files: {maps}"));
    let version = file
        .strip_prefix(SONAME)
        .and_then(|rest| rest.split('.').nth(1));
    let version = version.unwrap_or_else(|| panic!("no version in the file name {file}"));
    version
        .parse()
        .unwrap_or_else(|error| panic!("the version in {file}: {error}"))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{STATES_LATENCY_SINCE, states_latency, version};

    #[test]
    fn the_version_is_read_from_the_mapped_file_name() {
        let maps = "\
7f3a1b000000-7f3a1b00f000 r--p 00000000 08:01 1233 /usr/lib/x86_64-linux-gnu/libc.so.6
7f3a1c000000-7f3a1c00f000 r--p 00000000 08:01 1234 /usr/lib/x86_64-linux-gnu/libpipewire-0.3.so.0.1402.0
";
        assert_eq!(version(maps), 1402);
        let old = maps.replace("0.1402.0", "0.1005.0");
        assert!(version(&old) < STATES_LATENCY_SINCE);
    }

    #[test]
    fn the_first_library_that_states_a_latency_counts() {
        let dir = tempfile::tempdir().unwrap();
        let maps = dir.path().join("maps");
        for (version, states) in [
            (STATES_LATENCY_SINCE, true),
            (STATES_LATENCY_SINCE - 1, false),
        ] {
            let line = format!(
                "7f3a1c000000-7f3a1c00f000 r--p 00000000 08:01 1234 /usr/lib/libpipewire-0.3.so.0.{version}.0\n"
            );
            fs::write(&maps, line).unwrap();
            assert_eq!(states_latency(&maps), states, "{version}");
        }
    }
}
