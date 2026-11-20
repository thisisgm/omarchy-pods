#[test]
fn version_flag_prints_name_and_version() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_librepods"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("librepods {}\n", env!("CARGO_PKG_VERSION"))
    );
}
