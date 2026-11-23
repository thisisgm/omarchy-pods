//! Workspace checks and tools. `cargo xtask ci` runs the Rust gates of the hosted CI, or only the ones named; zizmor runs only there.

mod hw;
mod probe;
mod pw_ffi;

use std::env;
use std::ffi::OsString;
use std::process::{Command, ExitCode};

/// One gate: `cargo <name> <args>`.
#[derive(Clone, Copy, PartialEq)]
struct Gate {
    name: &'static str,
    args: &'static [&'static str],
}

/// The gates of `cargo xtask ci`, in the order they run.
const CI_STEPS: [Gate; 5] = [
    Gate {
        name: "fmt",
        args: &["--all", "--check"],
    },
    Gate {
        name: "clippy",
        args: &[
            "--workspace",
            "--all-targets",
            "--locked",
            "--",
            "-D",
            "warnings",
        ],
    },
    Gate {
        name: "nextest",
        args: &["run", "--workspace", "--locked"],
    },
    Gate {
        name: "test",
        args: &["--doc", "--workspace"],
    },
    Gate {
        name: "deny",
        args: &["check"],
    },
];

/// Set on every gate, as the hosted clippy job sets it, so a Cargo warning such as an unused dependency fails here too.
const BUILD_WARNINGS: (&str, &str) = ("CARGO_BUILD_WARNINGS", "deny");

fn main() -> ExitCode {
    match env::args_os().nth(1) {
        Some(command) if command == "ci" => match chosen(env::args_os().skip(2)) {
            Ok(gates) => run_ci(&gates),
            Err(unknown) => {
                report(&format!(
                    "xtask: no gate named {}; the gates are {}",
                    unknown.display(),
                    CI_STEPS.map(|gate| gate.name).join(", ")
                ));
                ExitCode::FAILURE
            }
        },
        Some(command) if command == "probe" => probe::run(env::args_os().skip(1)),
        Some(command) if command == "hw" => hw::run(env::args_os().skip(1)),
        Some(command) if command == "pw-ffi" && env::args_os().nth(2).is_none() => pw_ffi::run(),
        _ => {
            report(
                "usage: cargo xtask ci [gate]... | cargo xtask probe <command> | cargo xtask hw link --binary <path> | cargo xtask pw-ffi",
            );
            ExitCode::FAILURE
        }
    }
}

/// The gates named, each once and in their usual order, or every gate when none is; the error is the first name that is no gate.
// Sample input: `fmt clippy`.
fn chosen(names: impl Iterator<Item = OsString>) -> Result<Vec<Gate>, OsString> {
    let names: Vec<OsString> = names.collect();
    let is_gate = |name: &&OsString| CI_STEPS.iter().any(|gate| **name == *gate.name);
    if let Some(unknown) = names.iter().find(|name| !is_gate(name)) {
        return Err(unknown.clone());
    }
    let picked = |gate: &&Gate| names.is_empty() || names.iter().any(|name| *name == *gate.name);
    Ok(CI_STEPS.iter().filter(picked).copied().collect())
}

/// Runs `gates` in order and stops at the first one that fails.
fn run_ci(gates: &[Gate]) -> ExitCode {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    for gate in gates {
        let step = format!("cargo {} {}", gate.name, gate.args.join(" "));
        report(&format!("xtask: {step}"));
        let (name, value) = BUILD_WARNINGS;
        let status = match Command::new(&cargo)
            .arg(gate.name)
            .args(gate.args)
            .env(name, value)
            .status()
        {
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
    if gates == CI_STEPS {
        report("xtask: all checks passed");
    } else {
        report(&format!(
            "xtask: {} of {} gates ran and passed",
            gates.len(),
            CI_STEPS.len()
        ));
    }
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

/// The one place xtask writes a result for a shell to capture.
#[expect(
    clippy::print_stdout,
    reason = "a probe command's result is its standard output"
)]
fn print_line(line: &str) {
    println!("{line}");
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{BUILD_WARNINGS, CI_STEPS};

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
    fn every_local_gate_runs_in_the_hosted_workflow() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/ci.yml");
        let workflow = fs::read_to_string(&path).unwrap();
        let has_line = |wanted: &str| workflow.lines().any(|line| line.trim() == wanted);
        for gate in CI_STEPS {
            let args = gate.args.join(" ");
            let hosted = match gate.name {
                // The hosted job runs cargo-deny through its action, which takes the arguments as `command`.
                "deny" => {
                    assert!(workflow.contains("uses: EmbarkStudios/cargo-deny-action@"));
                    format!("command: {args}")
                }
                name => format!("- run: cargo {name} {args}"),
            };
            assert!(has_line(&hosted), "ci.yml has no `{hosted}`");
        }
        let (name, value) = BUILD_WARNINGS;
        let warnings = format!("{name}: {value}");
        assert!(has_line(&warnings), "ci.yml has no `{warnings}`");
    }

    #[test]
    fn the_hooks_run_these_gates() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../hk.pkl");
        let config = fs::read_to_string(&path).unwrap();
        let mut hook = "";
        let mut runs = Vec::new();
        // Sample input: `  ["pre-push"] {` opens a hook, `        check = "cargo xtask ci"` is one of its steps.
        for line in config.lines() {
            if let Some(name) = line
                .strip_prefix("  [\"")
                .and_then(|rest| rest.strip_suffix("\"] {"))
            {
                hook = name;
            }
            if line.trim_start().starts_with("check = \"cargo xtask") {
                runs.push((hook, line.split('"').nth(1).unwrap()));
            }
        }
        let wanted = [
            ("pre-commit", "cargo xtask ci fmt clippy"),
            ("pre-push", "cargo xtask ci"),
        ];
        assert_eq!(runs, wanted);
        for (_, command) in wanted {
            let names = command.strip_prefix("cargo xtask ci").unwrap();
            let gates = super::chosen(names.split_whitespace().map(Into::into));
            assert!(gates.is_ok(), "`{command}` names no gate of CI_STEPS");
        }
    }

    #[test]
    fn the_fenced_crates_match_the_workspace_lints_except_unsafe_code() {
        let workspace = lint_lines("../../Cargo.toml", "workspace.");
        let forbids = workspace.iter().filter(|line| *line == FORBID_UNSAFE);
        assert_eq!(forbids.count(), 1, "the workspace forbids unsafe code once");
        let expected: Vec<String> = workspace
            .iter()
            .map(|line| line.replace(FORBID_UNSAFE, DENY_UNSAFE))
            .collect();
        for manifest in [
            "../sys/Cargo.toml",
            "../trap/Cargo.toml",
            "../pw/Cargo.toml",
        ] {
            assert_eq!(lint_lines(manifest, ""), expected, "{manifest}");
        }
    }
}
