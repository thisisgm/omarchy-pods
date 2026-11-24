//! `cargo xtask release-check <file> <version>`: the four checks a daemon binary passes before it is published.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, ErrorKind, Read};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, ExitCode};

use rustix::fs::{Mode, OFlags};

use crate::{print_line, report};

/// The highest glibc symbol version a release may need: the hosted `ubuntu-24.04` image's glibc, older than Arch's.
const GLIBC_CEILING: [u32; 2] = [2, 39];

/// The libraries a release may need: glibc's own, the compiler's and `PipeWire`'s, and nothing else.
const ALLOWED_NEEDED: [&str; 5] = [
    "libc.so.6",
    "libm.so.6",
    "libgcc_s.so.1",
    "ld-linux-x86-64.so.2",
    "libpipewire-0.3.so.0",
];

/// The crate whose compile unit must carry a line table, so a backtrace through the daemon's own code has a file and a line.
const DAEMON_CRATE: &str = "librepods";

/// How long `--version` may take; the daemon answers well inside a second.
const VERSION_DEADLINE: &str = "10s";

/// How long a command that outlived the deadline has to stop after `SIGTERM` before `timeout` sends `SIGKILL`.
const STOP_GRACE: &str = "2s";

/// The status `timeout` exits with when the command stopped on `SIGTERM` after the deadline.
const TIMED_OUT: i32 = 124;

/// The signal `timeout` ends itself with when the command ended on `SIGKILL`, its own or another sender's.
const KILLED: i32 = 9;

/// The first bytes of every ELF file.
const ELF_MAGIC: &[u8] = b"\x7fELF";

const READELF: &str = "readelf";

pub(crate) fn run(file: &Path, version: &OsStr) -> ExitCode {
    let size = match elf_size(file) {
        Ok(size) => size,
        Err(error) => {
            report(&error);
            return ExitCode::FAILURE;
        }
    };
    let Some(dynamic) = readelf("-d", file) else {
        return ExitCode::FAILURE;
    };
    let Some(versions) = readelf("-V", file) else {
        return ExitCode::FAILURE;
    };
    // Without `no-follow-links` readelf also reads a debug file the binary links to, which is not shipped.
    let Some(debug_info) = readelf("--debug-dump=info,no-follow-links", file) else {
        return ExitCode::FAILURE;
    };
    let Some(line_programs) = readelf("--debug-dump=rawline,no-follow-links", file) else {
        return ExitCode::FAILURE;
    };
    let checks = [
        version_check(file, &version.to_string_lossy()),
        needed_check(&needed(&dynamic)),
        glibc_check(glibc_versions(&versions)),
        line_tables_check(&debug_info, &line_programs),
    ];
    for check in &checks {
        match check {
            Ok(line) => print_line(&format!("ok {line}")),
            Err(line) => print_line(&format!("FAIL {line}")),
        }
    }
    print_line(&format!("size: {size} bytes"));
    if checks.iter().all(Result::is_ok) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

// Sample input: b"\x7fELF" at the start of the file.
fn elf_size(path: &Path) -> Result<u64, String> {
    let unread = |error: io::Error| format!("xtask: cannot read {}: {error}", path.display());
    // A FIFO without a writer must not block before the file type check.
    let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd = rustix::fs::open(path, flags, Mode::empty()).map_err(|error| unread(error.into()))?;
    let mut file = File::from(fd);
    let metadata = file.metadata().map_err(unread)?;
    if !metadata.is_file() {
        return Err(format!("xtask: {} is not a regular file", path.display()));
    }

    let mut header = [0; ELF_MAGIC.len()];
    let is_elf = match file.read_exact(&mut header) {
        Ok(()) => header.as_slice() == ELF_MAGIC,
        Err(error) if error.kind() == ErrorKind::UnexpectedEof => false,
        Err(error) => return Err(unread(error)),
    };
    if is_elf {
        Ok(metadata.len())
    } else {
        Err(format!("xtask: {} is not an ELF file", path.display()))
    }
}

/// The text `readelf <flag>` writes for `file` in the C locale, or `None` once the failure is reported.
fn readelf(flag: &str, file: &Path) -> Option<String> {
    let output = match Command::new(READELF)
        .args(["--wide", flag])
        .arg(file)
        .env("LC_ALL", "C")
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            report(&format!(
                "xtask: `{READELF}` is not on the PATH; it comes with binutils"
            ));
            return None;
        }
        Err(error) => {
            report(&format!("xtask: could not start {READELF}: {error}"));
            return None;
        }
    };
    if !output.status.success() {
        report(&format!(
            "xtask: {READELF} {flag} failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Whether the last word `file --version` prints is `version`.
fn version_check(file: &Path, version: &str) -> Result<String, String> {
    // A bare name would be looked up on the PATH instead of read from this directory.
    let output = Command::new("timeout")
        .args(["--kill-after", STOP_GRACE, VERSION_DEADLINE])
        .arg(Path::new(".").join(file))
        .arg("--version")
        .output()
        .map_err(|error| format!("version: could not start timeout: {error}"))?;
    // Sample input: `librepods 2.0.0\n`.
    let said = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    // Standard error is shown only for a non-zero exit, the one path where it holds the reason.
    if output.status.code() == Some(TIMED_OUT) {
        return Err(format!(
            "version: `{} --version` did not exit within {VERSION_DEADLINE}",
            file.display()
        ));
    }
    // A command killed by anyone else before the deadline ends `timeout` the same way, so the line names both causes.
    if output.status.signal() == Some(KILLED) {
        return Err(format!(
            "version: `{} --version` ended on SIGKILL, which timeout sends {STOP_GRACE} after a missed {VERSION_DEADLINE} deadline",
            file.display()
        ));
    }
    if !output.status.success() {
        return Err(format!(
            "version: `{} --version` exited with {}: {}",
            file.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    if said.split_whitespace().last() == Some(version) {
        Ok(format!("version: {said}"))
    } else {
        Err(format!("version: `{said}` does not end with {version}"))
    }
}

/// The libraries `readelf -d` lists as needed, in its order.
// Sample input: ` 0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]`.
fn needed(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| line.contains("(NEEDED)"))
        .filter_map(|line| line.split_once('[')?.1.split_once(']'))
        .map(|(name, _)| name)
        .collect()
}

fn needed_check(names: &[&str]) -> Result<String, String> {
    match names.iter().find(|name| !ALLOWED_NEEDED.contains(name)) {
        Some(outside) => Err(format!("libraries: {outside} is not allowed")),
        None => Ok(format!("libraries: {}", names.join(" "))),
    }
}

/// The `GLIBC_` versions `readelf -V` lists as needed, each as its numbers, or the first name that is no version.
// Sample input: `  0x0010:   Name: GLIBC_2.34  Flags: none  Version: 3`.
fn glibc_versions(text: &str) -> Result<Vec<Vec<u32>>, String> {
    let mut versions = Vec::new();
    for line in text.lines() {
        let Some((_, rest)) = line.split_once("Name: GLIBC_") else {
            continue;
        };
        let name = rest.split_whitespace().next().unwrap_or_default();
        let numbers: Option<Vec<u32>> = name.split('.').map(|part| part.parse().ok()).collect();
        versions.push(numbers.ok_or_else(|| format!("GLIBC_{name}"))?);
    }
    Ok(versions)
}

fn glibc_name(numbers: &[u32]) -> String {
    let parts: Vec<String> = numbers.iter().map(u32::to_string).collect();
    format!("GLIBC_{}", parts.join("."))
}

fn glibc_check(versions: Result<Vec<Vec<u32>>, String>) -> Result<String, String> {
    let ceiling = glibc_name(&GLIBC_CEILING);
    let versions = versions.map_err(|name| format!("glibc: {name} is no version number"))?;
    match versions.iter().max() {
        None => Err("glibc: no GLIBC_ version is needed, so this is no glibc build".to_owned()),
        Some(highest) if highest.as_slice() <= GLIBC_CEILING.as_slice() => {
            Ok(format!("glibc: {}, at most {ceiling}", glibc_name(highest)))
        }
        Some(highest) => Err(format!("glibc: {} is above {ceiling}", glibc_name(highest))),
    }
}

/// The source of the compile unit `readelf --debug-dump=info` lists for crate `name`, if there is one.
// Sample input: `    <12>   DW_AT_name        : (strp) (offset: 0x42f629): crates/daemon/src/main.rs/@/librepods.f9d5f7b3489be63c-cgu.0`.
fn crate_source<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let unit = format!("{name}.");
    text.lines()
        .filter(|line| line.contains("DW_AT_name"))
        .filter_map(|line| line.rsplit_once(": ")?.1.split_once("/@/"))
        .find(|(_, unit_name)| unit_name.starts_with(&unit))
        .map(|(source, _)| source)
}

/// The `.debug_line` offset of the compile unit `readelf --debug-dump=info` lists for crate `name`, if it names one.
// Sample input: the unit's `DW_AT_name` line as above, then `    <16>   DW_AT_stmt_list   : 0x16a`.
fn crate_line_offset<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let unit = format!("/@/{name}.");
    let mut in_unit = false;
    for line in text.lines() {
        if line.contains("DW_TAG_compile_unit") {
            in_unit = false;
        } else if line.contains("DW_AT_name") && line.contains(&unit) {
            in_unit = true;
        } else if in_unit && line.contains("DW_AT_stmt_list") {
            return line.split_whitespace().last();
        }
    }
    None
}

/// The offset of each line program `readelf --debug-dump=rawline` lists.
// Sample input: `  Offset:                      0x16a`.
fn line_program_offsets(text: &str) -> Vec<&str> {
    text.lines()
        .filter_map(|line| line.trim_start().strip_prefix("Offset:"))
        .map(str::trim)
        .collect()
}

/// Passes when the daemon's compile unit points at a line program that `.debug_line` holds.
fn line_tables_check(debug_info: &str, line_programs: &str) -> Result<String, String> {
    let has_program = crate_line_offset(debug_info, DAEMON_CRATE)
        .is_some_and(|offset| line_program_offsets(line_programs).contains(&offset));
    match crate_source(debug_info, DAEMON_CRATE) {
        Some(source) if has_program => Ok(format!("line tables: {source}")),
        _ => Err(format!(
            "line tables: the {DAEMON_CRATE} crate has no line table, so a backtrace has no file and line"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        crate_line_offset, crate_source, glibc_check, glibc_versions, line_program_offsets, needed,
    };

    #[test]
    fn needed_reads_each_library_and_nothing_else() {
        let text = "Dynamic section at offset 0x4b9d40 contains 31 entries:\n  Tag        Type                         Name/Value\n 0x0000000000000001 (NEEDED)             Shared library: [libgcc_s.so.1]\n 0x0000000000000001 (NEEDED)             Shared library: [libc.so.6]\n 0x000000000000000c (INIT)               0x5000\n 0x000000000000000e (SONAME)             Library soname: [libother.so]\n";
        assert_eq!(needed(text), ["libgcc_s.so.1", "libc.so.6"]);
        assert_eq!(needed(""), Vec::<&str>::new());
    }

    #[test]
    fn glibc_versions_reads_the_glibc_names_only() {
        let text = "Version needs section '.gnu.version_r' contains 3 entries:\n  000000: Version: 1  File: libgcc_s.so.1  Cnt: 1\n  0x0010:   Name: GCC_3.0  Flags: none  Version: 5\n  0x0020: Version: 1  File: libc.so.6  Cnt: 2\n  0x0030:   Name: GLIBC_2.2.5  Flags: none  Version: 3\n  0x0040:   Name: GLIBC_2.34  Flags: none  Version: 2\n  000:   0 (*local*)       2 (GLIBC_2.41)\n";
        assert_eq!(glibc_versions(text), Ok(vec![vec![2, 2, 5], vec![2, 34]]));
        assert_eq!(glibc_versions(""), Ok(vec![]));
        let private = "  0x0050:   Name: GLIBC_PRIVATE  Flags: none  Version: 9\n";
        assert_eq!(glibc_versions(private), Err("GLIBC_PRIVATE".to_owned()));
    }

    #[test]
    fn the_ceiling_compares_numbers_not_text() {
        for (versions, passes) in [
            (vec![vec![2, 2, 5], vec![2, 39]], true),
            (vec![vec![2, 4]], true),
            (vec![vec![2, 39, 1]], false),
            (vec![vec![2, 40]], false),
            (vec![vec![3, 0]], false),
            (vec![], false),
        ] {
            assert_eq!(
                glibc_check(Ok(versions.clone())).is_ok(),
                passes,
                "{versions:?}"
            );
        }
    }

    #[test]
    fn crate_source_finds_only_the_named_crate() {
        let text = " <0><b>: Abbrev Number: 1 (DW_TAG_compile_unit)\n    <12>   DW_AT_name        : (indirect string, offset: 0x1b2e8e): /rustc/48a229ce/library/std/src/lib.rs/@/std.6c98fd8553dbae28-cgu.0\n    <30>   DW_AT_name        : (strp) (offset: 0x5): new\n    <44>   DW_AT_name        : (strp) (offset: 0x42f629): crates/daemon/src/main.rs/@/librepods_core.f9d5f7b3489be63c-cgu.0\n    <58>   DW_AT_name        : (strp) (offset: 0x42f629): crates/daemon/src/main.rs/@/librepods.f9d5f7b3489be63c-cgu.0\n";
        assert_eq!(
            crate_source(text, "librepods"),
            Some("crates/daemon/src/main.rs")
        );
        assert_eq!(
            crate_source(text, "std"),
            Some("/rustc/48a229ce/library/std/src/lib.rs")
        );
        assert_eq!(crate_source(text, "zbus"), None);
        assert_eq!(crate_source("", "librepods"), None);
    }

    #[test]
    fn the_line_offset_is_the_named_units_own() {
        let text = " <0><b>: Abbrev Number: 1 (DW_TAG_compile_unit)\n    <12>   DW_AT_name        : (strp) (offset: 0x1b2e8e): library/std/src/lib.rs/@/std.6c98fd8553dbae28-cgu.0\n    <16>   DW_AT_stmt_list   : 0\n <0><2c>: Abbrev Number: 1 (DW_TAG_compile_unit)\n    <38>   DW_AT_name        : (strp) (offset: 0x42f629): crates/daemon/src/main.rs/@/librepods.f9d5f7b3489be63c-cgu.0\n    <3c>   DW_AT_stmt_list   : 0x16a\n <0><5e>: Abbrev Number: 2 (DW_TAG_compile_unit)\n    <6a>   DW_AT_name        : (strp) (offset: 0x5): zbus/src/lib.rs/@/zbus.1-cgu.0\n";
        assert_eq!(crate_line_offset(text, "librepods"), Some("0x16a"));
        assert_eq!(crate_line_offset(text, "std"), Some("0"));
        assert_eq!(crate_line_offset(text, "zbus"), None);
        let programs = "Raw dump of debug contents of section .debug_line:\n\n  Offset:                      0\n  Length:                      102\n  Offset:                      0x16a\n";
        assert_eq!(line_program_offsets(programs), ["0", "0x16a"]);
        assert_eq!(line_program_offsets(""), Vec::<&str>::new());
    }
}
