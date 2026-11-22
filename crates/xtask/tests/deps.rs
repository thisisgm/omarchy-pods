//! The workspace's dependency fences, read from `cargo metadata` and from the source tree.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use serde_json::Value;

    /// Crates that exist only for tests and must never reach a binary.
    const TEST_CRATES: [&str; 5] = [
        "pods-samples",
        "pods-fake",
        "pods-trap",
        "pods-pwrig",
        "cpp-source",
    ];

    /// Every package of the workspace with the dependencies its manifest names.
    fn packages() -> Vec<Value> {
        let output = Command::new(env!("CARGO"))
            .args(["metadata", "--format-version", "1", "--no-deps", "--locked"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        // Sample input: `{"packages":[{"name":"pods-sys","dependencies":[{"name":"rustix","kind":null}]}]}`.
        let metadata: Value = serde_json::from_slice(&output.stdout).unwrap();
        metadata["packages"].as_array().unwrap().clone()
    }

    /// The `(dependency, kind)` pairs of one package; `kind` is `normal`, `dev` or `build`.
    fn dependencies(package: &Value) -> Vec<(String, String)> {
        let listed = package["dependencies"].as_array().unwrap();
        listed
            .iter()
            .map(|dependency| {
                let name = dependency["name"].as_str().unwrap().to_owned();
                let kind = dependency["kind"].as_str().unwrap_or("normal").to_owned();
                (name, kind)
            })
            .collect()
    }

    #[test]
    fn no_test_crate_is_a_normal_dependency() {
        for package in packages() {
            let owner = package["name"].as_str().unwrap();
            // A test crate may build on another test crate; any other package names one only for its tests.
            let owner_is_a_test_crate = TEST_CRATES.contains(&owner);
            for (name, kind) in dependencies(&package) {
                let test_crate = TEST_CRATES.contains(&name.as_str());
                assert!(
                    !test_crate || kind == "dev" || owner_is_a_test_crate,
                    "{owner} names the test crate {name} as a {kind} dependency"
                );
            }
        }
    }

    #[test]
    fn the_fake_does_not_use_the_protocol_crate() {
        for package in packages()
            .iter()
            .filter(|package| package["name"] == "pods-fake")
        {
            let names: Vec<String> = dependencies(package)
                .into_iter()
                .map(|(name, _)| name)
                .collect();
            assert!(
                !names.iter().any(|name| name == "pods-proto"),
                "pods-fake names pods-proto"
            );
        }
    }

    fn rust_files(dir: &Path, found: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_files(&path, found);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                found.push(path);
            }
        }
    }

    #[test]
    fn the_daemon_and_the_core_never_name_from_fd() {
        let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut found = Vec::new();
        for name in ["daemon", "core"] {
            rust_files(&crates.join(name), &mut found);
        }
        assert!(
            found.len() > 2,
            "no Rust files under the daemon and the core: {found:?}"
        );
        for path in found {
            let text = fs::read_to_string(&path).unwrap();
            assert!(
                !text.contains("from_fd"),
                "{} names from_fd",
                path.display()
            );
        }
    }
}
