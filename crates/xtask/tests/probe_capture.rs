//! The built binary's `probe capture` and `probe stop` commands against the fake pods, every L2CAP connect of the command taken by the socket trap, and `probe analyse` on the dictation runs they leave.

#[cfg(test)]
#[path = "capture/analysed.rs"]
mod analysed;
#[cfg(test)]
#[expect(
    dead_code,
    reason = "the parts of the fake pods only the unit tests of the capture loop read go unused here"
)]
#[path = "../src/probe/capture/tests/att_pods.rs"]
mod att_pods;
#[cfg(test)]
#[expect(
    dead_code,
    reason = "the home of an analysed run is read only where the run is decoded as well"
)]
#[path = "support/capture.rs"]
mod capture;
#[cfg(test)]
#[expect(
    dead_code,
    reason = "the parts of the fake pods only the unit tests of the capture loop read go unused here"
)]
#[path = "../src/probe/capture/tests/pods.rs"]
mod pods;

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use pods_proto::att::{self, CHARACTERISTIC_DECLARATION};
    use pods_proto::dictation::{DictationPacket, SESSION_ACTIVE, SESSION_IDLE};
    use pods_proto::doap::Handles;
    use pods_proto::packets::{
        ATT_PSM, CONTROL_PSM, HANDSHAKE, REQUEST_NOTIFICATIONS, SET_SPECIFIC_FEATURES,
    };
    use pods_proto::sdulog::Direction;
    use pods_proto::uplink::{START, STOP};
    use pods_trap::Outcome;
    use rustix::io::Errno;

    use super::att_pods::{
        BURST_PACKETS, DECLARED, DICTATION_START, DICTATION_STOP, MADE_UP_TYPE, declared_at,
    };
    use super::capture::{
        FAILED, HANG_LIMIT, ROOT_NAME, Running, STAMP, assert_att_ran, assert_control_logged,
        capture, far_end, files_of, hand_over, hand_over_after, home_with_device, kinds, logged,
        probe, read, refuse, route, run_path, stamp_of, transcript,
    };
    use super::pods::{self, Pods};

    /// The cue every soak prints when its stream starts.
    const SOAK_CUE: &str = "cue Stay as you are. Nothing to do.\n";
    /// How long the scripts let the active marker lead the dictation start write.
    const MARKER_LEAD: Duration = Duration::from_millis(480);
    /// How long the trap holds a dial of the attribute channel in the redial test: longer than that lead.
    const SLOW_DIAL: Duration = Duration::from_millis(600);
    /// The three packets of the handshake, in the order the command sends them.
    const HANDSHAKE_SENT: [&[u8]; 3] = [&HANDSHAKE, &SET_SPECIFIC_FEATURES, &REQUEST_NOTIFICATIONS];
    /// The first handle of the range the request for the declarations covers.
    const FIRST_HANDLE: u16 = 0x0001;
    /// The last handle of that range, the highest an attribute can have.
    const LAST_HANDLE: u16 = 0xffff;

    #[test]
    fn a_signal_mid_uplink_stops_the_uplink_and_fails_the_run() {
        let home = home_with_device();
        let (answer, handed) = hand_over();
        let command = capture(&home, "uplink", &["--script", "soak", "--minutes", "1"]);
        let running = Running::start(command, vec![route(CONTROL_PSM, answer)]);
        let flag = Arc::new(AtomicBool::new(false));
        let part = Pods {
            interrupt_after: Some(3),
            ..Pods::default()
        };
        let played = pods::spawn(far_end(&handed), part, Arc::clone(&flag));
        running.interrupt_within(HANG_LIMIT, &flag);
        let ran = running.finish();
        let pods = transcript(&played);

        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        assert_eq!(ran.stdout, SOAK_CUE);
        let said =
            "xtask probe: capture ended early: interrupted by a signal; STOP was sent (packets in ";
        assert!(ran.stderr.starts_with(said), "{}", ran.stderr);
        assert!(
            ran.stderr.ends_with(", START sent 1, STOP sent 1)\n"),
            "{}",
            ran.stderr
        );
        assert_eq!(ran.dials, [(CONTROL_PSM, Outcome::Accepted)]);
        assert_eq!(pods.got(), [&HANDSHAKE_SENT[..], &[&START, &STOP]].concat());
        let run = run_path(&home, "uplink");
        assert_eq!(files_of(&run), ["marks.tsv", "meta.txt", "sdu.log"]);
        assert_control_logged(&pods, &run);
        let marks = read(&run, "marks.tsv");
        assert!(
            marks.ends_with("\tend\tinterrupted by a signal\n"),
            "{marks}"
        );
        assert!(read(&run, "meta.txt").starts_with("script=soak\n"));
    }

    #[test]
    fn a_dictation_capture_maps_the_table_records_the_stream_and_stops_it_on_a_signal() {
        let home = home_with_device();
        let (control, control_far) = hand_over();
        let (att, att_far) = hand_over();
        let script = ["--route", "dictation", "--script", "soak", "--minutes", "1"];
        let command = capture(&home, "dictation", &script);
        let routes = vec![route(CONTROL_PSM, control), route(ATT_PSM, att)];
        let running = Running::start(command, routes);
        let flag = Arc::new(AtomicBool::new(false));
        let mut part = Pods::default();
        part.att.interrupt_after = Some(3);
        let played = pods::spawn_dialled(
            Some(far_end(&control_far)),
            att_far,
            part,
            Arc::clone(&flag),
        );
        running.interrupt_within(HANG_LIMIT, &flag);
        let ran = running.finish();
        let pods = transcript(&played);

        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        assert_eq!(ran.stdout, SOAK_CUE);
        let said = "xtask probe: capture ended early: interrupted by a signal; the dictation stop was sent (packets in ";
        assert!(ran.stderr.starts_with(said), "{}", ran.stderr);
        let counted = ", dictation start sent 1, dictation stop sent 1, requests refused 0)\n";
        assert!(ran.stderr.ends_with(counted), "{}", ran.stderr);
        let accepted = [
            (CONTROL_PSM, Outcome::Accepted),
            (ATT_PSM, Outcome::Accepted),
        ];
        assert_eq!(ran.dials, accepted);
        let control = [&HANDSHAKE_SENT[..], &[&SESSION_ACTIVE, &SESSION_IDLE]].concat();
        assert_eq!(pods.got(), control);
        let run = run_path(&home, "dictation");
        let files = ["att.log", "handles.tsv", "marks.tsv", "meta.txt", "sdu.log"];
        assert_eq!(files_of(&run), files);
        assert_att_ran(&pods, &run, &[&DICTATION_START, &DICTATION_STOP]);
        assert_control_logged(&pods, &run);
        let att_log = read(&run, "att.log");
        let notified = logged(&att_log, Direction::FromPods);
        let audio = notified.iter().filter(|pdu| {
            matches!(
                DictationPacket::parse(pdu, &Handles::OBSERVED),
                Ok(Some(DictationPacket::Audio(_)))
            )
        });
        // More audio than the burst a session opens with, so the stream ran on past it.
        assert!(audio.count() > BURST_PACKETS, "{att_log}");
        let marks = read(&run, "marks.tsv");
        for kind in [
            "handles",
            "active",
            "dictation-start",
            "started",
            "dictation-stop",
            "idle",
        ] {
            assert!(kinds(&marks).contains(&kind), "{kind} in\n{marks}");
        }
        assert!(marks.contains("\thandles\tpresent\n"), "{marks}");
        assert!(
            marks.ends_with("\tend\tinterrupted by a signal\n"),
            "{marks}"
        );
        let meta = read(&run, "meta.txt");
        assert!(
            meta.ends_with("\nroute=dictation\nno_marker=false\nunchecked=false\n"),
            "{meta}"
        );
    }

    #[test]
    fn a_slow_redial_of_the_attribute_channel_leaves_the_marker_its_lead() {
        let home = home_with_device();
        let (control, control_far) = hand_over();
        let (att, att_far) = hand_over_after(SLOW_DIAL);
        let script = ["--route", "dictation", "--script", "soak", "--minutes", "1"];
        let command = capture(&home, "redial", &[&script[..], &["--unchecked"]].concat());
        let routes = vec![route(CONTROL_PSM, control), route(ATT_PSM, att)];
        let running = Running::start(command, routes);
        let flag = Arc::new(AtomicBool::new(false));
        let mut part = Pods::default();
        // The pods answer no request of the walk, so the command closes the channel and dials a fresh one for the start write.
        part.att.answers = Some(0);
        part.att.interrupt_after = Some(3);
        let played = pods::spawn_dialled(
            Some(far_end(&control_far)),
            att_far,
            part,
            Arc::clone(&flag),
        );
        running.interrupt_within(HANG_LIMIT, &flag);
        let ran = running.finish();
        let pods = transcript(&played);

        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        let dialled_twice = [
            (CONTROL_PSM, Outcome::Accepted),
            (ATT_PSM, Outcome::Accepted),
            (ATT_PSM, Outcome::Accepted),
        ];
        assert_eq!(ran.dials, dialled_twice);
        assert!(pods.att_got.contains(&DICTATION_START.to_vec()));
        let marks = read(&run_path(&home, "redial"), "marks.tsv");
        assert!(marks.contains("\thandles\tunchecked\n"), "{marks}");
        let redialled_ns = stamp_of(&marks, "handles");
        assert!(stamp_of(&marks, "gate") >= redialled_ns, "{marks}");
        let lead = stamp_of(&marks, "dictation-start").saturating_sub(redialled_ns);
        assert!(u128::from(lead) >= MARKER_LEAD.as_nanos(), "{marks}");
    }

    #[test]
    fn a_capture_of_both_routes_stops_both_streams_on_a_signal() {
        let home = home_with_device();
        let (control, control_far) = hand_over();
        let (att, att_far) = hand_over();
        let command = capture(&home, "both", &["--route", "both", "--script", "both"]);
        let routes = vec![route(CONTROL_PSM, control), route(ATT_PSM, att)];
        let running = Running::start(command, routes);
        let flag = Arc::new(AtomicBool::new(false));
        // The flag rises three packets into the uplink, which the script starts five seconds after the dictation stream.
        let part = Pods {
            interrupt_after: Some(3),
            ..Pods::default()
        };
        let played = pods::spawn_dialled(
            Some(far_end(&control_far)),
            att_far,
            part,
            Arc::clone(&flag),
        );
        running.interrupt_within(HANG_LIMIT, &flag);
        let ran = running.finish();
        let pods = transcript(&played);

        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        assert_eq!(ran.stdout, "cue Count aloud to twenty\n");
        let said = "xtask probe: capture ended early: interrupted by a signal; STOP was sent; the dictation stop was sent (packets in ";
        assert!(ran.stderr.starts_with(said), "{}", ran.stderr);
        let accepted = [
            (CONTROL_PSM, Outcome::Accepted),
            (ATT_PSM, Outcome::Accepted),
        ];
        assert_eq!(ran.dials, accepted);
        let control = [
            &HANDSHAKE_SENT[..],
            &[&SESSION_ACTIVE, &START, &STOP, &SESSION_IDLE],
        ]
        .concat();
        assert_eq!(pods.got(), control);
        let run = run_path(&home, "both");
        let files = ["att.log", "handles.tsv", "marks.tsv", "meta.txt", "sdu.log"];
        assert_eq!(files_of(&run), files);
        assert_att_ran(&pods, &run, &[&DICTATION_START, &DICTATION_STOP]);
        assert_control_logged(&pods, &run);
        let meta = read(&run, "meta.txt");
        assert!(
            meta.starts_with("script=both\n") && meta.contains("\nroute=both\n"),
            "{meta}"
        );
    }

    #[test]
    fn an_att_map_capture_dials_the_attribute_channel_alone_and_writes_the_table() {
        let home = home_with_device();
        let (att, att_far) = hand_over();
        let command = capture(
            &home,
            "map",
            &["--route", "dictation", "--script", "att-map"],
        );
        let running = Running::start(command, vec![route(ATT_PSM, att)]);
        let never = Arc::new(AtomicBool::new(false));
        let played = pods::spawn_on(None, Some(far_end(&att_far)), Pods::default(), never);
        let ran = running.finish();
        let pods = transcript(&played);

        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        assert_eq!(ran.stdout, "");
        let pdus_in = pods.att_sent.len();
        assert_eq!(
            ran.stderr,
            format!(
                "xtask probe: capture complete: packets in 0, with audio 0, not logged 0, START sent 0, STOP sent 0, attribute packets in {pdus_in}, with audio 0, dictation start sent 0, dictation stop sent 0, requests refused 0\n"
            )
        );
        assert_eq!(ran.dials, [(ATT_PSM, Outcome::Accepted)]);
        assert_eq!(pods.got(), Vec::<&[u8]>::new());
        let run = run_path(&home, "map");
        let files = ["att.log", "handles.tsv", "marks.tsv", "meta.txt", "sdu.log"];
        assert_eq!(files_of(&run), files);
        let declarations =
            att::read_by_type_request(FIRST_HANDLE, LAST_HANDLE, CHARACTERISTIC_DECLARATION);
        assert_att_ran(&pods, &run, &[&declarations]);
        let listed = read(&run, "handles.tsv");
        let rows: Vec<&str> = listed
            .lines()
            .filter(|row| row.starts_with("declaration\t"))
            .collect();
        let kind = u16::from_le_bytes(MADE_UP_TYPE);
        let declared: Vec<String> = DECLARED
            .into_iter()
            .map(|(value, properties)| {
                let at = declared_at(value);
                format!("declaration\t{at:#06x}\t{properties:#04x}\t{value:#06x}\t{kind:#06x}")
            })
            .collect();
        assert_eq!(rows, declared, "{listed}");
        assert_eq!(read(&run, "sdu.log"), "");
        let marks = read(&run, "marks.tsv");
        assert!(marks.contains("\thandles\tpresent\n"), "{marks}");
    }

    #[test]
    fn stop_sends_the_stop_of_the_route_it_is_given() {
        let home = home_with_device();
        let (control, control_far) = hand_over();
        let command = probe(home.path(), &["stop"]);
        let running = Running::start(command, vec![route(CONTROL_PSM, control)]);
        let played = pods::spawn(far_end(&control_far), Pods::default(), Arc::default());
        let ran = running.finish();
        let pods = transcript(&played);
        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        assert_eq!(
            ran.stderr,
            "xtask probe: STOP was sent and the channel was kept open for 1000 ms\n"
        );
        assert_eq!(ran.stdout, "");
        assert_eq!(ran.dials, [(CONTROL_PSM, Outcome::Accepted)]);
        assert_eq!(pods.got(), [&HANDSHAKE_SENT[..], &[&STOP]].concat());

        let (control, control_far) = hand_over();
        let (att, att_far) = hand_over();
        let command = probe(home.path(), &["stop", "--route", "dictation"]);
        let routes = vec![route(CONTROL_PSM, control), route(ATT_PSM, att)];
        let running = Running::start(command, routes);
        let played = pods::spawn_dialled(
            Some(far_end(&control_far)),
            att_far,
            Pods::default(),
            Arc::default(),
        );
        let ran = running.finish();
        let pods = transcript(&played);
        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        assert_eq!(
            ran.stderr,
            "xtask probe: the dictation stop and the idle session marker were sent and the channels were kept open for 1000 ms\n"
        );
        assert_eq!(ran.stdout, "");
        let accepted = [
            (CONTROL_PSM, Outcome::Accepted),
            (ATT_PSM, Outcome::Accepted),
        ];
        assert_eq!(ran.dials, accepted);
        assert_eq!(pods.got(), [&HANDSHAKE_SENT[..], &[&SESSION_IDLE]].concat());
        assert_eq!(pods.att_got, [DICTATION_STOP]);
        // A stop keeps nothing: the session of init is still empty.
        let session = home.path().join(ROOT_NAME).join(STAMP);
        assert_eq!(fs::read_dir(session).unwrap().count(), 0);
    }

    #[test]
    fn a_refused_connect_is_named_and_starts_no_stream() {
        let home = home_with_device();
        let command = capture(&home, "page", &["--script", "soak", "--minutes", "1"]);
        let running = Running::start(command, vec![route(CONTROL_PSM, refuse(Errno::HOSTDOWN))]);
        let ran = running.finish();
        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        assert_eq!(
            ran.stderr,
            "xtask probe: could not connect the L2CAP channel: Host is down (os error 112)\n"
        );
        assert_eq!(ran.stdout, "");
        assert_eq!(
            ran.dials,
            [(CONTROL_PSM, Outcome::Refused(Errno::HOSTDOWN))]
        );
        let run = run_path(&home, "page");
        assert_eq!(files_of(&run), ["marks.tsv", "meta.txt", "sdu.log"]);
        assert_eq!(read(&run, "sdu.log"), "");
        let marks = read(&run, "marks.tsv");
        assert_eq!(kinds(&marks), ["end"], "{marks}");

        let (control, control_far) = hand_over();
        let script = ["--route", "dictation", "--script", "soak", "--minutes", "1"];
        let command = capture(&home, "refused", &script);
        let routes = vec![
            route(CONTROL_PSM, control),
            route(ATT_PSM, refuse(Errno::CONNREFUSED)),
        ];
        let running = Running::start(command, routes);
        let played = pods::spawn(far_end(&control_far), Pods::default(), Arc::default());
        let ran = running.finish();
        let pods = transcript(&played);
        assert_eq!(ran.code, FAILED, "{}", ran.stderr);
        let said = "xtask probe: capture ended early: the attribute channel on PSM 0x001f did not open: could not connect the L2CAP channel: Connection refused (os error 111) (packets in ";
        assert!(ran.stderr.starts_with(said), "{}", ran.stderr);
        assert_eq!(ran.stderr.lines().count(), 1, "{}", ran.stderr);
        assert_eq!(ran.stdout, "");
        let dials = [
            (CONTROL_PSM, Outcome::Accepted),
            (ATT_PSM, Outcome::Refused(Errno::CONNREFUSED)),
        ];
        assert_eq!(ran.dials, dials);
        assert_eq!(pods.got(), HANDSHAKE_SENT);
        let run = run_path(&home, "refused");
        let files = ["att.log", "handles.tsv", "marks.tsv", "meta.txt", "sdu.log"];
        assert_eq!(files_of(&run), files);
        assert_eq!(read(&run, "att.log"), "");
        assert_eq!(read(&run, "handles.tsv"), "");
        let marks = read(&run, "marks.tsv");
        assert!(!kinds(&marks).contains(&"active"), "{marks}");
        assert!(
            marks.ends_with("Connection refused (os error 111)\n"),
            "{marks}"
        );
    }
}
