//! The built binary's `probe capture --steps --allow-control` and its uplink starts of other codecs, against the fake pods under the socket trap.

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
    reason = "the analysed run, the cut helpers and the cue helpers serve the other capture tests"
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
    use pods_proto::sdulog;
    use pods_samples::uplink::{START, STOP};

    use super::capture::{
        OWNER_FILE, after_handshake, control_step, home_with_device, plan, read, refusal_of,
        run_path, run_steps, step_marks, steps_args,
    };
    use super::pods::Pods;

    /// The control commands a Mac sends at connect, one a line with no wait between them.
    const CONTROLS: &str = "control 0400040009000601000000\ncontrol 0400040009003202000000\ncontrol 0400040009003c02000000\ncontrol 0400040009001301000000\ncontrol 0400040009001201000000\n";
    /// Where the START's codec id, a u32 little endian, begins.
    const CODEC_AT: usize = 12;
    /// The uplink START with codec id 257 in place of 130.
    const START_257: [u8; 19] = {
        let mut packet = START;
        let [c0, c1, c2, c3] = 257u32.to_le_bytes();
        packet[CODEC_AT] = c0;
        packet[CODEC_AT + 1] = c1;
        packet[CODEC_AT + 2] = c2;
        packet[CODEC_AT + 3] = c3;
        packet
    };
    /// What the command says of a control command sent without `--allow-control`.
    const SETTING_COMMAND: &str = "a control command, opcode 0x0009, changes a device setting the run cannot restore, so no step may send one without --allow-control";

    // Sample input: `control 0400040009000601000000`, a line of `CONTROLS`, read as the packet it sends.
    fn packet_of(line: &str) -> Vec<u8> {
        sdulog::parse_hex(line.strip_prefix("control ").unwrap()).unwrap()
    }

    #[test]
    fn a_control_command_is_refused_without_allow_control() {
        let home = home_with_device();
        let path = plan(home.path(), "plan", CONTROLS.as_bytes(), OWNER_FILE);
        let said = refusal_of(&home, &steps_args(&path));
        let expected = format!(
            "xtask probe: {}: line 1: {SETTING_COMMAND}\n",
            path.display()
        );
        assert_eq!(said, expected);
    }

    #[test]
    fn allow_control_sends_each_control_command_as_written_and_marks_its_line() {
        let home = home_with_device();
        let path = plan(home.path(), "plan", CONTROLS.as_bytes(), OWNER_FILE);
        let args = [&steps_args(&path)[..], &["--allow-control"]].concat();
        let (ran, pods) = run_steps(&home, "control", &args, Pods::default());

        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        let sent: Vec<Vec<u8>> = CONTROLS.lines().map(packet_of).collect();
        assert_eq!(after_handshake(&pods), sent);
        let marks = read(&run_path(&home, "control"), "marks.tsv");
        let texts: Vec<String> = step_marks(&marks)
            .iter()
            .map(|(_, text)| text.to_string())
            .collect();
        let expected: Vec<String> = (1..=sent.len())
            .map(|line| format!("line {line} control"))
            .collect();
        assert_eq!(texts, expected, "{marks}");
    }

    #[test]
    fn allow_control_with_a_script_is_refused_and_nothing_is_sent() {
        let home = home_with_device();
        let said = refusal_of(&home, &["--script", "knock", "--allow-control"]);
        let refused = "the argument '--script <SCRIPT>' cannot be used with '--allow-control'";
        assert!(said.contains(refused), "{said}");
    }

    #[test]
    fn a_start_with_another_codec_runs_under_the_uplink_guard_and_its_stop() {
        let (codec, exact) = (control_step(&START_257), control_step(&START));
        let alone: [&[u8]; 2] = [&START_257, &STOP];
        let then_exact: [&[u8]; 3] = [&START_257, &START, &STOP];
        let cases = [
            (format!("{codec}wait 50\n"), &alone[..]),
            (format!("{codec}wait 50\n{exact}wait 50\n"), &then_exact[..]),
        ];
        for (contents, expected) in cases {
            let home = home_with_device();
            let path = plan(home.path(), "plan", contents.as_bytes(), OWNER_FILE);
            let (ran, pods) = run_steps(&home, "codec", &steps_args(&path), Pods::default());

            assert_eq!(ran.code, Some(0), "{}", ran.stderr);
            assert_eq!(after_handshake(&pods), expected);
            let marks = read(&run_path(&home, "codec"), "marks.tsv");
            assert!(
                marks.contains("\tstart\tuplink start codec 257\n"),
                "{marks}"
            );
            assert!(marks.contains("\tstop\tuplink stop\n"), "{marks}");
            assert!(step_marks(&marks).is_empty(), "{marks}");
        }
    }
}
