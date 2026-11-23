//! `cargo xtask pw-ffi` through the built binary, with a stand-in for the generator on a private PATH.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::{Command, Output};

    /// What the generator gets between the formatter settings and the allow list: the shape of the file.
    const SHAPE: &str = "--rust-target\n1.85\n--rust-edition\n2024\n--no-prepend-enum-name\n--no-layout-tests\n--no-doc-comments\n--merge-extern-blocks\n";

    /// Runs the command and `extra` arguments with `dir` as the whole PATH.
    fn pw_ffi(dir: &Path, extra: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_xtask"))
            .arg("pw-ffi")
            .args(extra)
            .env("PATH", dir)
            .output()
            .unwrap()
    }

    /// Puts a `bindgen` in `dir` that writes its arguments, one a line, to `bindgen.args` there and exits with `code`.
    fn stand_in(dir: &Path, code: u8) {
        let path = dir.join("bindgen");
        let args = dir.join("bindgen.args");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexit {code}\n",
            args.display()
        );
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The names the recorded arguments give after each `flag`.
    // Sample input: `--allowlist-function\npw_init\n`: the flag on one line, its name on the next.
    fn named<'a>(args: &'a str, flag: &str) -> Vec<&'a str> {
        args.split(&format!("{flag}\n"))
            .skip(1)
            .filter_map(|rest| rest.lines().next())
            .collect()
    }

    #[test]
    fn no_generator_on_the_path_is_said_and_fails() {
        let dir = tempfile::tempdir().unwrap();
        let output = pw_ffi(dir.path(), &[]);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "xtask: `bindgen` is not on the PATH; it comes with the crate bindgen-cli\n"
        );
    }

    #[test]
    fn a_generator_that_fails_is_reported_with_its_status() {
        let dir = tempfile::tempdir().unwrap();
        stand_in(dir.path(), 3);
        let output = pw_ffi(dir.path(), &[]);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "xtask: bindgen failed with exit status: 3\n"
        );
    }

    #[test]
    fn an_argument_is_refused_before_the_generator_runs() {
        let dir = tempfile::tempdir().unwrap();
        stand_in(dir.path(), 0);
        let output = pw_ffi(dir.path(), &["--check"]);
        assert_eq!(output.status.code(), Some(1));
        let said = String::from_utf8_lossy(&output.stderr);
        assert!(said.starts_with("usage: cargo xtask "), "{said}");
        assert!(
            !dir.path().join("bindgen.args").exists(),
            "the generator ran"
        );
    }

    #[test]
    fn the_generator_gets_the_header_the_output_the_shape_and_the_allow_lists() {
        let dir = tempfile::tempdir().unwrap();
        stand_in(dir.path(), 0);
        let output = pw_ffi(dir.path(), &[]);
        assert!(output.status.success(), "{output:?}");
        let args = fs::read_to_string(dir.path().join("bindgen.args")).unwrap();
        let committed = Path::new(env!("CARGO_MANIFEST_DIR")).join("../pw/src/ffi.rs");
        let declared = fs::read_to_string(&committed).unwrap();
        assert!(
            args.starts_with("/usr/include/pipewire-0.3/pipewire/pipewire.h\n--output\n"),
            "{args}"
        );
        let format = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../rustfmt.toml");
        assert!(format.is_file(), "{}", format.display());
        let middle = format!(
            "--output\n{}\n--rustfmt-configuration-file\n{}\n{SHAPE}--allowlist-function\n",
            committed.display(),
            format.display()
        );
        assert!(args.contains(&middle), "{args}");
        assert!(
            args.ends_with("--\n-I/usr/include/pipewire-0.3\n-I/usr/include/spa-0.2\n"),
            "{args}"
        );
        // Sample input: `    pub fn pw_init(argc: *mut ::std::os::raw::c_int, argv: *mut *mut *mut ::std::os::raw::c_char);`.
        let mut functions: Vec<&str> = declared
            .lines()
            .filter_map(|line| line.trim().strip_prefix("pub fn "))
            .filter_map(|rest| rest.split('(').next())
            .collect();
        let mut allowed = named(&args, "--allowlist-function");
        functions.sort_unstable();
        allowed.sort_unstable();
        assert_eq!(
            functions, allowed,
            "the committed file against the allow list"
        );
        let types = named(&args, "--allowlist-type");
        assert_eq!(types, ["spa_io_rate_match", "spa_io_type"]);
        let macros = named(&args, "--allowlist-var");
        assert_eq!(macros, ["SPA_IO_RATE_MATCH_FLAG_ACTIVE"]);
    }
}
