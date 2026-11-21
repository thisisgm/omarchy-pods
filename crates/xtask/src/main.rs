//! Workspace checks. `cargo xtask ci` runs the Rust gates of the hosted CI; zizmor runs only there.

use std::env;
use std::process::{Command, ExitCode};

/// The gates of `cargo xtask ci`, in the order they run.
const CI_STEPS: [&[&str]; 5] = [
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
    &["test", "--doc", "--workspace"],
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    const FORBID_UNSAFE: &str = r#"unsafe_code = "forbid""#;
    const DENY_UNSAFE: &str = r#"unsafe_code = "deny""#;

    /// The lint tables of one manifest, headers included, without blank lines and comments.
    // Sample input: `[workspace.lints.rust]` then `unsafe_code = "forbid"`, read with prefix `workspace.`.
    fn lint_lines(manifest: &str, prefix: &str) -> Vec<String> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(manifest);
        let text = fs::read_to_string(&path).unwrap();
        let header = format!("[{prefix}lints.");
        let mut in_lints = false;
        let mut lines = Vec::new();
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                in_lints = line.starts_with(&header);
            }
            if in_lints && !line.is_empty() && !line.starts_with('#') {
                lines.push(line.replacen(&header, "[lints.", 1));
            }
        }
        lines
    }

    #[test]
    fn sys_lint_table_matches_the_workspace_except_unsafe_code() {
        let workspace = lint_lines("../../Cargo.toml", "workspace.");
        let sys = lint_lines("../sys/Cargo.toml", "");
        let forbids = workspace.iter().filter(|line| *line == FORBID_UNSAFE);
        assert_eq!(forbids.count(), 1, "the workspace forbids unsafe code once");
        let expected: Vec<String> = workspace
            .iter()
            .map(|line| line.replace(FORBID_UNSAFE, DENY_UNSAFE))
            .collect();
        assert_eq!(sys, expected);
    }
}
