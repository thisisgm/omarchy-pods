//! Workspace checks. `cargo xtask ci` runs the Rust gates of the hosted CI; zizmor runs only there.

use std::env;
use std::process::{Command, ExitCode};

/// The gates of `cargo xtask ci`, in the order they run.
const CI_STEPS: [&[&str]; 4] = [
    &["fmt", "--all", "--check"],
    &[
        "clippy",
        "--workspace",
        "--all-targets",
        "--locked",
        "--",
        "-D",
        "warnings",
    ],
    &["nextest", "run", "--workspace", "--locked"],
    &["deny", "check"],
];

fn main() -> ExitCode {
    if env::args().nth(1).as_deref() == Some("ci") {
        run_ci()
    } else {
        report("usage: cargo xtask ci");
        ExitCode::FAILURE
    }
}

/// Runs every gate in order and stops at the first one that fails.
fn run_ci() -> ExitCode {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    for args in CI_STEPS {
        let step = format!("cargo {}", args.join(" "));
        report(&format!("xtask: {step}"));
        let status = match Command::new(&cargo).args(args).status() {
            Ok(status) => status,
            Err(error) => {
                report(&format!("xtask: could not start {step}: {error}"));
                return ExitCode::FAILURE;
            }
        };
        if !status.success() {
            report(&format!("xtask: {step} failed with {status}"));
            return ExitCode::FAILURE;
        }
    }
    report("xtask: all checks passed");
    ExitCode::SUCCESS
}

/// The one place xtask writes progress and errors.
#[expect(
    clippy::print_stderr,
    reason = "xtask is a terminal tool and reports on stderr"
)]
fn report(message: &str) {
    eprintln!("{message}");
}
