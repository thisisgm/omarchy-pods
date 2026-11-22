//! The proximity keys through the built daemon: asked for once per link, kept owner only in `keys.json`, never shown anywhere else.

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
#[expect(
    dead_code,
    reason = "each suite uses part of the shared support, so the rest goes unused here"
)]
mod support;

#[cfg(test)]
mod tests {
    use std::fmt::Write;
    use std::fs::{self, DirBuilder, Permissions};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    use std::path::Path;
    use std::str;

    use pods_fake::Profile;
    use pods_samples::{CASE, Cell, IDENTITY_KEY, MODEL_NUMBER, PAYLOAD_KEY, RIGHT, host, keys_of};

    use super::support::bus::{BUS_NAME, INTERFACE, Listener, OBJECT_PATH, changes_rule};
    use super::support::rig::{Device, Rig, connected};
    use super::support::{DIR_MODE, FILE_MODE, HANG_LIMIT, Session, wait_for};

    const UNREADABLE: &str = "keys: cannot read";

    const CANNOT_WRITE: &str = "keys: cannot write";

    /// Left bud levels no profile starts with; each arrives in the status after every packet sent before it was handled.
    const MARKS: [u8; 2] = [42, 43];

    /// The low byte of the payload entry's big endian length: header 6, count 1, identity entry 20, payload tag 1, high byte 1.
    const PAYLOAD_LEN_LOW_AT: usize = 6 + 1 + 20 + 1 + 1;

    /// A mode the daemon never sets, so a pair written in place would keep it.
    const READABLE_MODE: u32 = 0o644;

    /// Base64 turns each group of 3 bytes into 4 digits.
    const BASE64_GROUP_BYTES: usize = 3;

    const BASE64_GROUP_DIGITS: usize = 4;

    const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    fn reversed(key: [u8; 16]) -> [u8; 16] {
        let mut key = key;
        key.reverse();
        key
    }

    fn keyed() -> Profile {
        Profile {
            keys: Some((IDENTITY_KEY, PAYLOAD_KEY)),
            ..Profile::default()
        }
    }

    fn hex(key: &[u8]) -> String {
        let mut text = String::new();
        for byte in key {
            write!(text, "{byte:02x}").unwrap();
        }
        text
    }

    /// The file as the daemon writes it.
    fn file_text(identity: [u8; 16], payload: [u8; 16]) -> String {
        format!(
            "{{\"identity\":\"{}\",\"payload\":\"{}\"}}\n",
            hex(&identity),
            hex(&payload)
        )
    }

    /// Padded with `=`, as `base64 -w0` prints it.
    fn base64(bytes: &[u8]) -> String {
        let mut text = String::new();
        for chunk in bytes.chunks(3) {
            let mut group = [0_u8; 3];
            group[..chunk.len()].copy_from_slice(chunk);
            let bits = u32::from_be_bytes([0, group[0], group[1], group[2]]);
            for (at, shift) in [18, 12, 6, 0].into_iter().enumerate() {
                let sextet = usize::try_from((bits >> shift) & 0x3f).unwrap();
                text.push(if at <= chunk.len() {
                    char::from(BASE64[sextet])
                } else {
                    '='
                });
            }
        }
        text
    }

    /// The base64 digits that depend only on `key` when it sits `offset` bytes into a longer buffer.
    fn base64_inside(key: [u8; 16], offset: usize) -> Vec<u8> {
        let mut buffer = vec![0_u8; offset];
        buffer.extend(key);
        let text = base64(&buffer).into_bytes();
        let first = offset.div_ceil(BASE64_GROUP_BYTES);
        let last = buffer.len() / BASE64_GROUP_BYTES;
        let digits = |groups: usize| groups.checked_mul(BASE64_GROUP_DIGITS).unwrap();
        text[digits(first)..digits(last)].to_vec()
    }

    /// Each form a key could take in output, alone or inside a longer buffer: raw, hex, base64 and the lists `Debug` prints.
    fn forms(key: [u8; 16]) -> Vec<Vec<u8>> {
        let decimal = format!("{key:?}");
        let hex_list = format!("{key:02x?}");
        let mut forms = vec![
            key.to_vec(),
            hex(&key).into_bytes(),
            hex(&key).to_uppercase().into_bytes(),
            decimal.trim_matches(['[', ']']).as_bytes().to_vec(),
            hex_list.trim_matches(['[', ']']).as_bytes().to_vec(),
        ];
        forms.extend((0..BASE64_GROUP_BYTES).map(|offset| base64_inside(key, offset)));
        forms
    }

    fn assert_holds_no_key(what: &str, output: &[u8]) {
        let keys = [IDENTITY_KEY, PAYLOAD_KEY];
        let all = keys.into_iter().chain(keys.map(reversed));
        for form in all.flat_map(forms) {
            let found = output.windows(form.len()).any(|window| window == form);
            assert!(!found, "{what} holds a key as {form:02x?}");
        }
    }

    fn plant_keys(session: &Session, bytes: &[u8]) {
        let path = session.keys_file();
        DirBuilder::new()
            .recursive(true)
            .mode(DIR_MODE)
            .create(path.parent().unwrap())
            .unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn wait_keys_file(session: &Session, wanted: &str) {
        wait_for(&format!("keys.json to hold {wanted:?}"), || {
            (fs::read_to_string(session.keys_file()).ok()? == wanted).then_some(())
        });
    }

    /// The inode and the modification time, which a replace through a renamed copy both change.
    fn identity_of(path: &Path) -> (u64, i64, i64) {
        let metadata = fs::metadata(path).unwrap();
        (metadata.ino(), metadata.mtime(), metadata.mtime_nsec())
    }

    fn wait_requests(device: &Device, count: usize) {
        let request = host::request_proximity_keys();
        device
            .pods
            .wait_heard(&request, count, HANG_LIMIT)
            .unwrap_or_else(|deadline| panic!("{count} key requests not heard: {deadline:?}"));
    }

    fn requests_heard(device: &Device) -> usize {
        let request = host::request_proximity_keys();
        let heard = device.pods.heard();
        heard.iter().filter(|heard| heard.sdu == request).count()
    }

    /// Sends a battery report with the left level `level` and waits for it, so the daemon has handled every packet sent before it.
    fn mark(rig: &Rig, device: &Device, level: u8) {
        let left = Cell {
            level,
            charging: false,
        };
        device.pods.set_battery(left, RIGHT, CASE);
        rig.wait_status(&format!("the left level {level}"), |status| {
            status["left"]["level"] == level
        });
    }

    /// Stops the daemon, which writes every pair it kept before it exits.
    fn stop(rig: &mut Rig) {
        let status = rig.stop(HANG_LIMIT);
        assert!(status.success(), "the daemon exited with {status}");
    }

    /// Starts the daemon of `rig` with pods of `profile` and links them until the key request is heard.
    fn link(mut rig: Rig, profile: Profile) -> (Rig, Device) {
        let device = rig.add_pods(profile);
        rig.start();
        rig.link_up(&device);
        rig.wait_filled();
        wait_requests(&device, 1);
        (rig, device)
    }

    /// Pods that never answer the request get `reply` instead; the daemon is stopped once it handled it.
    fn after_reply(reply: &[u8]) -> Rig {
        let (mut rig, device) = link(Rig::new(), Profile::default());
        device.pods.send(reply);
        mark(&rig, &device, MARKS[0]);
        assert!(connected(&rig.status()), "a bad reply stopped the link");
        stop(&mut rig);
        assert_holds_no_key("the daemon's log", rig.daemon().log().as_bytes());
        rig
    }

    #[test]
    fn the_key_request_ends_the_opening_once_per_link() {
        let (rig, device) = link(Rig::new(), Profile::default());
        let heard: Vec<Vec<u8>> = device
            .pods
            .heard()
            .into_iter()
            .map(|heard| heard.sdu)
            .collect();
        let opening = [
            host::handshake(),
            host::set_specific_features(),
            host::request_notifications(),
            host::request_proximity_keys(),
        ];
        assert_eq!(heard, opening);
        mark(&rig, &device, MARKS[0]);
        assert_eq!(requests_heard(&device), 1, "the link asked twice");
        rig.disconnect();
        rig.relink(&device);
        wait_requests(&device, 2);
        mark(&rig, &device, MARKS[1]);
        assert_eq!(requests_heard(&device), 2, "the second link asked twice");
        rig.finish();
    }

    #[test]
    fn the_keys_file_appears_owner_only_after_the_reply() {
        let mut rig = Rig::new();
        rig.unmask();
        let (rig, device) = link(rig, Profile::default());
        mark(&rig, &device, MARKS[0]);
        assert!(
            !rig.session().keys_file().exists(),
            "keys.json before any reply"
        );
        device.pods.send(&keys_of(IDENTITY_KEY, PAYLOAD_KEY));
        let path = rig.session().keys_file();
        wait_keys_file(rig.session(), &file_text(IDENTITY_KEY, PAYLOAD_KEY));
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, FILE_MODE);
        // The state file made the directory at start; this checks the keys sit in an owner only one.
        let dir_mode = fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(dir_mode & 0o777, DIR_MODE);
        rig.finish();
    }

    #[test]
    fn the_same_keys_again_do_not_rewrite_the_file() {
        let (mut rig, device) = link(Rig::new(), keyed());
        let path = rig.session().keys_file();
        wait_keys_file(rig.session(), &file_text(IDENTITY_KEY, PAYLOAD_KEY));
        let written = identity_of(&path);
        rig.disconnect();
        rig.relink(&device);
        wait_requests(&device, 2);
        mark(&rig, &device, MARKS[0]);
        stop(&mut rig);
        assert_eq!(
            identity_of(&path),
            written,
            "the same pair was written again"
        );
        rig.finish();
    }

    #[test]
    fn a_stored_pair_read_at_start_is_not_written_again() {
        let rig = Rig::new();
        plant_keys(
            rig.session(),
            file_text(IDENTITY_KEY, PAYLOAD_KEY).as_bytes(),
        );
        let path = rig.session().keys_file();
        let planted = identity_of(&path);
        let (mut rig, device) = link(rig, keyed());
        mark(&rig, &device, MARKS[0]);
        stop(&mut rig);
        assert_eq!(
            identity_of(&path),
            planted,
            "the stored pair was written again"
        );
        assert!(
            !rig.daemon().log().contains(UNREADABLE),
            "{}",
            rig.daemon().log()
        );
        rig.finish();
    }

    #[test]
    fn a_stored_pair_others_can_read_is_made_owner_only_at_start() {
        let rig = Rig::new();
        plant_keys(
            rig.session(),
            file_text(IDENTITY_KEY, PAYLOAD_KEY).as_bytes(),
        );
        let path = rig.session().keys_file();
        fs::set_permissions(&path, Permissions::from_mode(READABLE_MODE)).unwrap();
        let planted = identity_of(&path);
        let (mut rig, device) = link(rig, keyed());
        mark(&rig, &device, MARKS[0]);
        stop(&mut rig);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            FILE_MODE
        );
        assert_eq!(
            identity_of(&path),
            planted,
            "the stored pair was written again"
        );
        rig.finish();
    }

    #[test]
    fn a_different_pair_replaces_the_stored_one() {
        let rig = Rig::new();
        let other = (reversed(IDENTITY_KEY), reversed(PAYLOAD_KEY));
        plant_keys(rig.session(), file_text(other.0, other.1).as_bytes());
        let path = rig.session().keys_file();
        fs::set_permissions(&path, Permissions::from_mode(READABLE_MODE)).unwrap();
        let (rig, device) = link(rig, keyed());
        wait_keys_file(rig.session(), &file_text(IDENTITY_KEY, PAYLOAD_KEY));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            FILE_MODE
        );
        device.pods.send(&keys_of(other.0, other.1));
        wait_keys_file(rig.session(), &file_text(other.0, other.1));
        rig.finish();
    }

    #[test]
    fn no_output_of_the_daemon_holds_key_bytes() {
        let (mut rig, device) = link(Rig::new(), keyed());
        wait_keys_file(rig.session(), &file_text(IDENTITY_KEY, PAYLOAD_KEY));
        mark(&rig, &device, MARKS[0]);
        let listener = Listener::subscribe(rig.session_bus(), &changes_rule());
        let properties = "org.freedesktop.DBus.Properties";
        let reply = listener.call(BUS_NAME, OBJECT_PATH, properties, "GetAll", &(INTERFACE,));
        assert_holds_no_key("the D-Bus properties", reply.unwrap().data().bytes());
        let status = rig.client(&["status"]);
        assert!(status.status.success(), "librepods status: {status:?}");
        assert_holds_no_key("status", &status.stdout);
        let state = fs::read(rig.session().state_file()).unwrap();
        assert_holds_no_key("the state file", &state);
        stop(&mut rig);
        // The daemon has one log level, so its whole standard error is the most verbose output it has.
        assert_holds_no_key("the daemon's log", rig.daemon().log().as_bytes());
        rig.finish();
    }

    #[test]
    fn a_short_reply_stores_nothing() {
        let mut reply = keys_of(IDENTITY_KEY, PAYLOAD_KEY);
        reply.pop();
        let rig = after_reply(&reply);
        assert!(
            !rig.session().keys_file().exists(),
            "a short reply was stored"
        );
        rig.finish();
    }

    #[test]
    fn a_reply_with_a_wrong_entry_length_stores_nothing() {
        let mut reply = keys_of(IDENTITY_KEY, PAYLOAD_KEY);
        reply[PAYLOAD_LEN_LOW_AT] = 0x0f;
        let rig = after_reply(&reply);
        assert!(
            !rig.session().keys_file().exists(),
            "a wrong entry length was stored"
        );
        rig.finish();
    }

    #[test]
    fn pods_that_never_answer_fill_their_state_as_before() {
        let (mut rig, device) = link(Rig::new(), Profile::default());
        let status = rig.status();
        assert_eq!(status["device_name"], Profile::default().name);
        assert_eq!(status["model_number"], MODEL_NUMBER);
        assert_eq!(status["left"]["level"], Profile::default().left.level);
        mark(&rig, &device, MARKS[0]);
        stop(&mut rig);
        assert!(
            !rig.session().keys_file().exists(),
            "keys.json with no reply"
        );
        rig.finish();
    }

    #[test]
    fn a_corrupt_keys_file_is_ignored() {
        let rig = Rig::new();
        let whole = file_text(IDENTITY_KEY, PAYLOAD_KEY).into_bytes();
        let cut = &whole[..whole.len() / 2];
        plant_keys(rig.session(), cut);
        let (mut rig, _device) = link(rig, Profile::default());
        let log = rig.daemon().log();
        let lines: Vec<&str> = log.lines().filter(|line| line.contains("keys:")).collect();
        let expected = format!(
            "{UNREADABLE} {}: not a JSON object",
            rig.session().keys_file().display()
        );
        assert_eq!(lines, [expected], "{log}");
        assert_holds_no_key("the daemon's log", log.as_bytes());
        assert!(!log.contains(str::from_utf8(cut).unwrap()), "{log}");
        rig.finish();
    }

    #[test]
    fn a_corrupt_keys_file_is_replaced_by_the_next_reply() {
        let rig = Rig::new();
        plant_keys(rig.session(), b"{\"identity\":\"zz\"}\n");
        let (rig, _device) = link(rig, keyed());
        wait_keys_file(rig.session(), &file_text(IDENTITY_KEY, PAYLOAD_KEY));
        rig.finish();
    }

    #[test]
    fn a_write_that_failed_is_tried_again_on_the_next_reply() {
        let rig = Rig::new();
        let path = rig.session().keys_file();
        // A directory where the file goes, so the rename over it fails.
        DirBuilder::new()
            .recursive(true)
            .mode(DIR_MODE)
            .create(&path)
            .unwrap();
        let (mut rig, device) = link(rig, keyed());
        let daemon = rig.daemon();
        wait_for("the failed write to be logged", || {
            daemon.log().contains(CANNOT_WRITE).then_some(())
        });
        assert_holds_no_key("the daemon's log", daemon.log().as_bytes());
        fs::remove_dir(&path).unwrap();
        rig.disconnect();
        rig.relink(&device);
        wait_keys_file(rig.session(), &file_text(IDENTITY_KEY, PAYLOAD_KEY));
        rig.finish();
    }
}
