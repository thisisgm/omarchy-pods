#[test]
fn a_wear_edge_reports_a_failed_paired_get() {
    let mut rig = keyed();
    let _device = rig.add_pods(Profile::default());
    rig.start();
    wait_intake(&rig, "listening");
    let out = marked_out();
    let advertiser = rig.bluez().advertise(&out.address, &out.data);
    rig.wait_status("the out-of-ear advert", |status| {
        picked(status)["case"]["level"] == 10
    });
    rig.bluez().fail_calls(true);
    advertiser.update(&ours(disconnected_worn_header(), PAYLOAD).data);
    let error = wait_for("the failed pairing check", || {
        rig.daemon()
            .log()
            .lines()
            .find_map(|line| line.strip_prefix("bluez: Connect failed: "))
            .map(str::to_owned)
    });
    rig.bluez().fail_calls(false);
    assert_eq!(error, "BlueZ Paired read failed: org.bluez.Error.Failed");
    assert!(wear_call(&rig).is_none());
    rig.finish();
}

#[test]
fn a_wear_edge_omits_the_path_from_a_standard_dbus_error() {
    let mut rig = keyed();
    let _device = rig.add_pods(Profile::default());
    rig.start();
    wait_intake(&rig, "listening");
    let out = marked_out();
    let advertiser = rig.bluez().advertise(&out.address, &out.data);
    rig.wait_status("the out-of-ear advert", |status| {
        picked(status)["case"]["level"] == 10
    });
    rig.bluez().fail_paired_read();
    advertiser.update(&ours(disconnected_worn_header(), PAYLOAD).data);
    let error = "org.freedesktop.DBus.Error.UnknownObject";
    let failed = format!("bluez: Connect failed: BlueZ Paired read failed: {error}");
    let log = wait_for("the failed pairing check", || {
        let log = rig.daemon().log();
        log.contains(&failed).then_some(log)
    });
    assert!(!log.contains("/org/bluez"), "object path in log:\n{log}");
    assert!(!log.contains("dev_"), "device identifier in log:\n{log}");
    assert!(wear_call(&rig).is_none());
    rig.finish();
}
