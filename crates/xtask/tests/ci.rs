//! `cargo xtask ci` with gates named, through the built binary.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use std::process::{Command, Output};

    fn ci(gates: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_xtask"))
            .arg("ci")
            .args(gates)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap()
    }

    #[test]
    fn a_gate_named_twice_runs_once_and_alone() {
        let output = ci(&["fmt", "fmt"]);
        let said = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{said}");
        assert_eq!(said.matches("xtask: cargo ").count(), 1, "{said}");
        assert!(said.contains("xtask: cargo fmt --all --check"), "{said}");
        assert!(
            said.contains("xtask: 1 of 6 gates ran and passed"),
            "{said}"
        );
    }

    #[test]
    fn a_name_that_is_no_gate_runs_nothing() {
        let output = ci(&["fmt", "lint"]);
        let said = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{said}");
        assert_eq!(
            said,
            "xtask: no gate named lint; the gates are fmt, clippy, nextest, test, deny, and bench when it is named\n"
        );
    }
}
