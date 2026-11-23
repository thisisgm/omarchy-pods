//! `cargo xtask pw-ffi`: writes the declarations `pods-pw` calls, from the installed `PipeWire` headers.

use std::io::ErrorKind;
use std::path::Path;
use std::process::{Command, ExitCode};

use crate::report;

/// The generator; the hosted job that runs this command installs it at the version the committed file names.
const GENERATOR: &str = "bindgen";
/// The header that includes every declaration below.
const HEADER: &str = "/usr/include/pipewire-0.3/pipewire/pipewire.h";
/// Where the development packages of Debian, Ubuntu and Arch put the two header trees.
const INCLUDE_DIRS: [&str; 2] = ["/usr/include/pipewire-0.3", "/usr/include/spa-0.2"];
/// The committed file, from this crate's directory.
const OUTPUT: &str = "../pw/src/ffi.rs";
/// The formatter settings of the workspace, so `cargo fmt` leaves the file as written.
const FORMAT: &str = "../../rustfmt.toml";

/// Every function `pods-pw` calls; the types they name come with them.
const FUNCTIONS: [&str; 20] = [
    "pw_init",
    "pw_thread_loop_new",
    "pw_thread_loop_start",
    "pw_thread_loop_stop",
    "pw_thread_loop_destroy",
    "pw_thread_loop_lock",
    "pw_thread_loop_unlock",
    "pw_thread_loop_get_loop",
    "pw_context_new",
    "pw_context_destroy",
    "pw_context_connect_fd",
    "pw_core_disconnect",
    "pw_properties_new_dict",
    "pw_stream_new",
    "pw_stream_destroy",
    "pw_stream_add_listener",
    "pw_stream_connect",
    "pw_stream_disconnect",
    "pw_stream_dequeue_buffer",
    "pw_stream_queue_buffer",
];
/// The area the process callback steers the converter through and the id it arrives under, which no function names.
const TYPES: [&str; 2] = ["spa_io_rate_match", "spa_io_type"];
/// The flag that makes the converter read the area, a macro beside the struct.
const MACROS: [&str; 1] = ["SPA_IO_RATE_MATCH_FLAG_ACTIVE"];

/// The first compiler with edition 2024, which the generator asks for before it writes that edition.
const EDITION_SINCE: &str = "1.85";

/// Constants without their enum's name in front, no layout tests and no copied header comments: only what is called.
const SHAPE: [&str; 8] = [
    "--rust-target",
    EDITION_SINCE,
    "--rust-edition",
    "2024",
    "--no-prepend-enum-name",
    "--no-layout-tests",
    "--no-doc-comments",
    "--merge-extern-blocks",
];

pub(crate) fn run() -> ExitCode {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut command = Command::new(GENERATOR);
    command
        .arg(HEADER)
        .arg("--output")
        .arg(here.join(OUTPUT))
        .arg("--rustfmt-configuration-file")
        .arg(here.join(FORMAT))
        .args(SHAPE);
    for (flag, names) in [
        ("--allowlist-function", &FUNCTIONS[..]),
        ("--allowlist-type", &TYPES[..]),
        ("--allowlist-var", &MACROS[..]),
    ] {
        for name in names {
            command.args([flag, name]);
        }
    }
    command.arg("--");
    for dir in INCLUDE_DIRS {
        command.arg(format!("-I{dir}"));
    }
    // The generator's standard error is this command's, so a header it cannot read is named there.
    match command.status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => {
            report(&format!("xtask: {GENERATOR} failed with {status}"));
            ExitCode::FAILURE
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            report(&format!(
                "xtask: `{GENERATOR}` is not on the PATH; it comes with the crate bindgen-cli"
            ));
            ExitCode::FAILURE
        }
        Err(error) => {
            report(&format!("xtask: could not start {GENERATOR}: {error}"));
            ExitCode::FAILURE
        }
    }
}
