//! The built binary's `probe source --replay` on a private audio server: a generated capture goes through the decode chain and the stream source into a stock recorder.

#[cfg(test)]
#[expect(
    dead_code,
    reason = "the store's shapes and the commands on a run go unused here"
)]
#[path = "support/run.rs"]
mod run;

// Inside cfg(test) so the lint table lets helpers unwrap, as it lets the tests themselves.
#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read};
    use std::path::{Path, PathBuf};
    use std::process::{Child, ExitStatus, Stdio};
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    use pods_mic::decode::{Decoded, Decoder};
    use pods_mic::level::MIC_GAIN_DB;
    use pods_mic::{NODE_LATENCY_MS, RATE_HZ, SAMPLES_PER_UNIT, STAMP_STEP, UPLINK_PERIOD_MS};
    use pods_proto::sdulog::{Direction, Record};
    use pods_proto::uplink::{AUDIO_HEADER_LEN, START};
    use pods_pwrig::tone::{CHECKED, SETTLE, assert_tone, frames};
    use pods_pwrig::{Recorder, Server};
    use pods_samples::uplink::{audio_sdu, silent_unit, tone_unit};
    use rustix::process::{Pid, Signal, kill_process};
    use serde_json::{Value, json};

    use super::run::{UNITS_PER_SDU, log_text, put_file};

    const NODE: &str = "omapods_input.probe";
    /// Line 15 of 480 stands near 1 kHz, far under the half rate of the slowest recorder.
    const LINE: u16 = 15;
    /// A level that decodes to a peak near a fourteenth of full scale, which the node's gain raises to about a third: under the limiter's knee, so the recording is the tone whole.
    const LEVEL: u8 = 180;
    /// Longer than any test records, so the replay is still feeding when the recorder closes its file.
    const CAPTURE_PACKETS: u32 = 700;
    /// A capture whose last packet lands at 2.97 s, so only a node kept while the reader plays its tail sees the line at 3 s.
    const SHORT_PACKETS: u32 = 100;
    const NS_PER_MS: u64 = 1_000_000;
    /// The largest gap between two packets seen from real pods.
    const WORST_GAP_MS: u32 = 72;
    /// How late the packet after that gap lands.
    const LATE_MS: u32 = WORST_GAP_MS - UPLINK_PERIOD_MS;
    /// Every tenth packet comes after that gap.
    const GAP_EVERY: u32 = 10;
    const LAST_BEFORE_GAP: u32 = GAP_EVERY - 1;
    /// The packet that stalls, three seconds in, when the recorder is linked; it and every later one land that much late.
    const STALL_AT: u32 = 100;
    /// Twice the delay the reader holds, so the reader runs dry before the stalled packet lands.
    const STALL_MS: u32 = 2 * NODE_LATENCY_MS;
    /// Recorded past the stall and the landing of its packet.
    const STALL_RECORDED: Duration = Duration::from_secs(5);
    /// A signal is seen within one poll of 10 ms; the rest is slack for a loaded host.
    const SIGNAL_SEEN_WITHIN: Duration = Duration::from_secs(1);
    /// What the gap test records: ten of its gaps.
    const GAPS_RECORDED: Duration = Duration::from_secs(3);
    /// Recorded beyond the settle and the checked span, for the silence before the first sound.
    const LEAD: Duration = Duration::from_millis(200);
    /// Silence a tape must end in once the capture's last unit has fallen away: well under the reads that carry it out.
    const SILENT_END: Duration = Duration::from_millis(2);
    /// Silent packets that land first, about a second, so the recorder is linked before the tone begins.
    const SILENT_PACKETS: u32 = 34;
    /// How far the tone's recorded span may stand from the capture's at each end: the resampler's edges, well under one unit.
    const SPAN_SLACK: Duration = Duration::from_millis(1);
    /// A local server and a loaded host answer well inside this; past it something hung.
    const WITHIN: Duration = Duration::from_secs(10);
    /// A replay that prints no line and does not end for this long has hung.
    const HANG_LIMIT: Duration = Duration::from_secs(30);
    /// The peak, of full scale, of the tone the capture's units decode to; the node's gain leaves it under the limiter's knee.
    const TONE_AMPLITUDE: f64 = 0.0695;
    /// How far the decoder's own output may stand from that tone, and the recording from it.
    const OWN_TOLERANCE_DB: f64 = 0.1;
    const ENERGY_TOLERANCE_DB: f64 = 1.0;
    /// A recorded sample this close to zero counts as none: under one thousandth of full scale.
    const NEAR_ZERO: i16 = 32;
    /// Units decoded for the chain's own output: more than the tone check reads.
    const OWN_UNITS: usize = 300;
    /// The first library whose streams state a process latency.
    const STATES_LATENCY_SINCE: [u32; 3] = [1, 3, 81];
    const UNREAD: &str = "distance none, rate none, unsteered N, underruns N, concealed N, decode errors N, decoder resets N";
    const READ: &str = "distance N ms, rate N ppm, unsteered N, underruns N, concealed N, decode errors N, decoder resets N";
    const FAILED: Option<i32> = Some(1);

    /// A capture of tone packets, packet `k` landing `land_ms(k)` after the first.
    fn capture(packets: u32, land_ms: impl Fn(u32) -> u32) -> Vec<Record> {
        let units = vec![tone_unit(LINE, LEVEL); usize::try_from(UNITS_PER_SDU).unwrap()];
        let record = |packet: u32| Record {
            at_ns: u64::from(land_ms(packet)).saturating_mul(NS_PER_MS),
            direction: Direction::FromPods,
            sdu: audio_sdu(packet.wrapping_mul(UNITS_PER_SDU * STAMP_STEP), &units),
        };
        (0..packets).map(record).collect()
    }

    fn on_time(packet: u32) -> u32 {
        packet.saturating_mul(UPLINK_PERIOD_MS)
    }

    /// Every tenth packet lands `WORST_GAP_MS` after the one before it, and the packet due meanwhile lands with it.
    fn with_gaps(packet: u32) -> u32 {
        let before = packet.saturating_sub(1);
        match packet % GAP_EVERY {
            LAST_BEFORE_GAP => on_time(packet).saturating_add(LATE_MS),
            0 if packet > 0 => on_time(before).saturating_add(LATE_MS),
            _ => on_time(packet),
        }
    }

    /// The capture as the store keeps one: a file of the owner alone.
    fn kept(dir: &Path, records: &[Record]) -> PathBuf {
        put_file(dir, "sdu.log", log_text(records).as_bytes());
        dir.join("sdu.log")
    }

    struct Ran {
        status: ExitStatus,
        lines: Vec<String>,
        stderr: String,
    }

    /// The built command on a private server; dropping it kills and reaps the command, then ends the thread that reads it.
    struct Replay {
        child: Child,
        printed: Receiver<String>,
        reader: Option<JoinHandle<()>>,
    }

    impl Replay {
        fn start(server: &Server, home: &Path, capture: &Path) -> Self {
            let mut command = server.command(env!("CARGO_BIN_EXE_xtask"));
            command
                .args(["probe", "source", "--replay"])
                .arg(capture)
                .env("HOME", home)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = command.spawn().unwrap();
            let stdout = BufReader::new(child.stdout.take().unwrap());
            let (lines, printed) = mpsc::channel();
            let reader = thread::spawn(move || {
                for line in stdout.lines().map_while(Result::ok) {
                    // The receiver is gone only when the test already failed.
                    drop(lines.send(line));
                }
            });
            Self {
                child,
                printed,
                reader: Some(reader),
            }
        }

        fn interrupt(&self) {
            kill_process(Pid::from_child(&self.child), Signal::INT).unwrap();
        }

        /// Every line the command printed and how it ended, once its standard output closes.
        fn finish(mut self) -> Ran {
            let mut lines = Vec::new();
            loop {
                match self.printed.recv_timeout(HANG_LIMIT) {
                    Ok(line) => lines.push(line),
                    Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => {
                        panic!("no line and no end within {HANG_LIMIT:?}; printed {lines:?}")
                    }
                }
            }
            // Standard output closed, so the command has exited or is exiting.
            let status = self.child.wait().unwrap();
            let mut stderr = String::new();
            let mut errors = self.child.stderr.take().unwrap();
            errors.read_to_string(&mut stderr).unwrap();
            Ran {
                status,
                lines,
                stderr,
            }
        }
    }

    impl Drop for Replay {
        fn drop(&mut self) {
            // A command that already exited refuses the kill, and the wait then reaps nothing.
            drop(self.child.kill());
            drop(self.child.wait());
            if let Some(reader) = self.reader.take() {
                drop(reader.join());
            }
        }
    }

    // Sample input: `distance 0.412 ms, rate -82.4 ppm, unsteered 0, underruns 2, ...`, from which `underruns` gives 2.
    fn field(line: &str, name: &str) -> f64 {
        let after = line.split_once(&format!("{name} ")).unwrap().1;
        let number = after.split([' ', ',']).next().unwrap();
        number.parse().unwrap()
    }

    // Sample input: `distance 0.412 ms, rate -82.4 ppm, unsteered 0, ...`, which gives `distance N ms, rate N ppm, unsteered N, ...`.
    fn shape(line: &str) -> String {
        let word = |word: &str| {
            let (text, comma) = match word.strip_suffix(',') {
                Some(text) => (text, ","),
                None => (word, ""),
            };
            let text = if text.parse::<f64>().is_ok_and(f64::is_finite) {
                "N"
            } else {
                text
            };
            format!("{text}{comma}")
        };
        let words: Vec<String> = line.split(' ').map(word).collect();
        words.join(" ")
    }

    /// The pitch of the tone: steady units at one line sound one line above it.
    fn tone_hz() -> f64 {
        let lines_per_second = f64::from(RATE_HZ) / f64::from(2 * SAMPLES_PER_UNIT);
        f64::from(LINE + 1) * lines_per_second
    }

    /// What the decoder makes of the capture's units, as a 16 bit recorder would hold it.
    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "a value inside full scale, rounded to the nearest sample"
    )]
    fn own_output() -> Vec<i16> {
        let mut decoder = Decoder::new().unwrap();
        let unit = tone_unit(LINE, LEVEL);
        let mut samples = Vec::new();
        for _ in 0..OWN_UNITS {
            let Decoded::Samples(frame) = decoder.unit(&unit) else {
                panic!("the decoder refused a tone unit");
            };
            let held = frame
                .iter()
                .map(|&sample| (sample * f32::from(i16::MAX)).round() as i16);
            samples.extend(held);
        }
        samples
    }

    // Sample input: `the recorder had not read the capture's end 1000 ms after it was due, so ...`, which gives 1000 ms.
    fn tail_bound(stderr: &str) -> Duration {
        let after = stderr.split_once("the capture's end ").expect(stderr).1;
        let ms = after.split(' ').next().unwrap();
        Duration::from_millis(ms.parse().unwrap())
    }

    /// Where the recording's first sound is.
    fn first_sound(recorded: &[i16]) -> usize {
        let found = recorded.iter().position(|sample| sample.abs() > NEAR_ZERO);
        found.expect("the recording holds no sound")
    }

    /// The longest run of samples near zero after the first sound.
    fn longest_silence(recorded: &[i16]) -> usize {
        let sounding = &recorded[first_sound(recorded)..];
        let silences = sounding.split(|sample| sample.abs() > NEAR_ZERO);
        silences.map(<[i16]>::len).max().unwrap_or(0)
    }

    /// Whether the server's library states a process latency.
    fn states_latency(server: &Server) -> bool {
        let dump = server.dump();
        let objects = dump.as_array().unwrap();
        // Sample input: `"1.4.2"`, the `info.version` of the core object.
        let version = objects
            .iter()
            .find_map(|object| object.pointer("/info/version").and_then(Value::as_str))
            .expect("the dump holds no core version");
        let parts: Vec<u32> = version
            .split('.')
            .map(|part| part.parse().unwrap())
            .collect();
        parts.as_slice() >= STATES_LATENCY_SINCE.as_slice()
    }

    /// How many numbers in `value`, at any depth, equal `wanted`.
    fn numbers_equal(value: &Value, wanted: u64) -> usize {
        match value {
            Value::Number(number) => usize::from(number.as_u64() == Some(wanted)),
            Value::Array(items) => items.iter().map(|item| numbers_equal(item, wanted)).sum(),
            Value::Object(fields) => fields
                .values()
                .map(|item| numbers_equal(item, wanted))
                .sum(),
            Value::Null | Value::Bool(_) | Value::String(_) => 0,
        }
    }

    /// The capture replayed into a recorder linked once the node is up, recorded for `span`, then the replay interrupted; gives how it ran, the tape and the tape's rate.
    fn recorded(records: &[Record], span: Duration) -> (Ran, Vec<i16>, u32) {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let replay = Replay::start(&server, dir.path(), &kept(dir.path(), records));
        server.wait_node(NODE, WITHIN);
        let recorder = Recorder::superwhisper(&server, NODE, &dir.path().join("recorded.wav"));
        server.link(NODE, recorder.node());
        let rate = recorder.rate();
        recorder.wait_frames(frames(LEAD.saturating_add(span), rate), WITHIN);
        let tape = recorder.finish();
        replay.interrupt();
        let ran = replay.finish();
        assert!(ran.status.success(), "{}", ran.stderr);
        (ran, tape, rate)
    }

    #[test]
    fn a_replayed_capture_is_recorded_by_a_real_recorder() {
        let (_, tape, rate) = recorded(&capture(CAPTURE_PACKETS, on_time), SETTLE + CHECKED);

        // The recording holds the tone the decoder itself makes of the units, whole, at its pitch and raised by the node's gain.
        assert_tone(
            &own_output(),
            RATE_HZ,
            tone_hz(),
            TONE_AMPLITUDE,
            OWN_TOLERANCE_DB,
        );
        let raised = TONE_AMPLITUDE * 10_f64.powf(f64::from(MIC_GAIN_DB) / 20.0);
        assert_tone(&tape, rate, tone_hz(), raised, ENERGY_TOLERANCE_DB);
    }

    #[test]
    fn a_recording_to_the_captures_end_holds_its_last_unit_and_ends_in_silence() {
        // An 85 ms cycle keeps a 60 ms driver stall from releasing two overdue cycles at once.
        const TAIL_GRAPH_QUANTUM: u32 = 4096;
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start_with_quantum(dir.path(), TAIL_GRAPH_QUANTUM);
        let mut records = capture(SILENT_PACKETS + SHORT_PACKETS, on_time);
        let silent = vec![silent_unit(); usize::try_from(UNITS_PER_SDU).unwrap()];
        for (record, packet) in records.iter_mut().zip(0..SILENT_PACKETS) {
            record.sdu = audio_sdu(packet.wrapping_mul(UNITS_PER_SDU * STAMP_STEP), &silent);
        }
        let capture = kept(dir.path(), &records);
        let replay = Replay::start(&server, dir.path(), &capture);
        server.wait_node(NODE, WITHIN);
        let recorder = Recorder::superwhisper(&server, NODE, &dir.path().join("recorded.wav"));
        server.link(NODE, recorder.node());
        let rate = recorder.rate();
        let ran = replay.finish();
        assert!(ran.status.success(), "{}", ran.stderr);
        assert_eq!(ran.stderr, "");
        let tape = recorder.finish();
        let start = first_sound(&tape);
        assert!(start > 0, "the recorder was linked after the tone began");
        let end = tape
            .iter()
            .rposition(|sample| sample.abs() > NEAR_ZERO)
            .unwrap();
        let sounding = end.saturating_sub(start).saturating_add(1);
        let unit = Duration::from_secs(u64::from(SAMPLES_PER_UNIT)) / RATE_HZ;
        let tone = unit * SHORT_PACKETS * UNITS_PER_SDU;
        // Once the reader passes the last unit it falls away over one more unit at most.
        let not_before = frames(tone.checked_sub(SPAN_SLACK).unwrap(), rate);
        let not_after = frames(tone + unit + SPAN_SLACK, rate);
        assert!(
            (not_before..=not_after).contains(&sounding),
            "the tone sounds for {sounding} samples, not {not_before} to {not_after}"
        );
        // The last unit sounds and falls away, so the tape ends in silence rather than inside the tone.
        let quiet = tape
            .iter()
            .rev()
            .take_while(|sample| sample.abs() <= NEAR_ZERO);
        let silent_end = quiet.count();
        assert!(
            silent_end >= frames(SILENT_END, rate),
            "the tape of {} samples ends with {silent_end} near zero",
            tape.len()
        );
    }

    #[test]
    fn a_recorder_gone_before_the_end_fails_the_replay_once_the_tail_bound_passes() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let capture = kept(dir.path(), &capture(SHORT_PACKETS, on_time));
        let started = Instant::now();
        let replay = Replay::start(&server, dir.path(), &capture);
        server.wait_node(NODE, WITHIN);
        let recorder = Recorder::superwhisper(&server, NODE, &dir.path().join("recorded.wav"));
        server.link(NODE, recorder.node());
        recorder.wait_frames(frames(LEAD, recorder.rate()), WITHIN);
        // Nothing reads the node from here on, so no read ever passes the capture's end.
        drop(recorder.finish());
        let ran = replay.finish();
        let took = started.elapsed();
        assert_eq!(ran.status.code(), FAILED, "{}", ran.stderr);
        let bound = tail_bound(&ran.stderr);
        // The replay begins after its start, so its last packet, the node's latency and the bound all pass first.
        let last = Duration::from_millis(u64::from(on_time(SHORT_PACKETS - 1) + NODE_LATENCY_MS));
        let due = last + bound;
        assert!(
            took >= due && took <= due + WITHIN,
            "the replay ended {took:?} after its start, due {due:?}"
        );
        server.wait_no_node(NODE, WITHIN);
    }

    #[test]
    fn the_node_reports_its_latency() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let capture = kept(dir.path(), &capture(CAPTURE_PACKETS, on_time));
        let replay = Replay::start(&server, dir.path(), &capture);
        server.wait_node(NODE, WITHIN);
        let recorder = Recorder::superwhisper(&server, NODE, &dir.path().join("recorded.wav"));
        // The link makes the node's port, which could repeat the figure.
        server.link(NODE, recorder.node());
        let node = server.wait_node(NODE, Duration::ZERO);
        let ns = u64::from(NODE_LATENCY_MS) * NS_PER_MS;
        // An older library keeps the parameter to itself and shows an empty list.
        let (stated, times) = if states_latency(&server) {
            (json!([{"quantum": 0.0, "rate": 0, "ns": ns}]), 1)
        } else {
            (json!([]), 0)
        };
        assert_eq!(node.pointer("/info/params/ProcessLatency"), Some(&stated));
        let description = node.pointer("/info/props/node.description");
        assert_eq!(description, Some(&json!("AirPods Microphone (probe)")));
        // No session manager prefers the probe's node over a device.
        let priority = node.pointer("/info/props/priority.session");
        assert_eq!(priority, Some(&json!(0)));
        let dump = server.dump();
        assert_eq!(numbers_equal(&dump, ns), times, "{dump}");
        replay.interrupt();
        let ran = replay.finish();
        assert!(ran.status.success(), "{}", ran.stderr);
    }

    #[test]
    fn a_replayed_gap_of_72_ms_has_no_underrun() {
        let (ran, tape, rate) = recorded(&capture(CAPTURE_PACKETS, with_gaps), GAPS_RECORDED);
        let last = ran.lines.last().expect("no status line");
        assert_eq!(shape(last), READ);
        for line in &ran.lines {
            let counted = ["underruns", "concealed", "decode errors"].map(|name| field(line, name));
            assert_eq!(counted, [0.0; 3], "{line}");
        }
        // One unit of the stream, at the recorder's rate.
        let unit = usize::from(SAMPLES_PER_UNIT) * usize::try_from(rate).unwrap()
            / usize::try_from(RATE_HZ).unwrap();
        let silence = longest_silence(&tape);
        assert!(silence < unit, "{silence} samples near zero in a row");
    }

    #[test]
    fn a_replayed_stall_past_the_cover_is_an_underrun() {
        let stalled = |packet| {
            let late = if packet >= STALL_AT { STALL_MS } else { 0 };
            on_time(packet).saturating_add(late)
        };
        let (ran, _, _) = recorded(&capture(CAPTURE_PACKETS, stalled), STALL_RECORDED);
        let last = ran.lines.last().expect("no status line");
        assert!(field(last, "underruns") >= 1.0, "{last}");
    }

    #[test]
    fn sigint_ends_the_replay_and_removes_the_node() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let records = capture(CAPTURE_PACKETS, on_time);
        let replay = Replay::start(&server, dir.path(), &kept(dir.path(), &records));
        server.wait_node(NODE, WITHIN);
        let signalled = Instant::now();
        replay.interrupt();
        let ran = replay.finish();
        let took = signalled.elapsed();
        assert!(ran.status.success(), "{}", ran.stderr);
        assert_eq!(ran.stderr, "");
        assert!(took < SIGNAL_SEEN_WITHIN, "{took:?}");
        server.wait_no_node(NODE, WITHIN);
    }

    #[test]
    fn a_server_that_goes_away_ends_the_replay_with_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let records = capture(CAPTURE_PACKETS, on_time);
        let replay = Replay::start(&server, dir.path(), &kept(dir.path(), &records));
        server.wait_node(NODE, WITHIN);
        server.stop();
        let ran = replay.finish();
        assert_eq!(ran.status.code(), FAILED, "{}", ran.stderr);
        assert!(
            ran.stderr.contains("the audio server went away"),
            "{}",
            ran.stderr
        );
    }

    #[test]
    fn a_status_line_with_no_recorder_holds_no_distance_and_numbers_otherwise() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        let capture = kept(dir.path(), &capture(SHORT_PACKETS, on_time));
        let ran = Replay::start(&server, dir.path(), &capture).finish();
        assert!(ran.status.success(), "{}", ran.stderr);
        assert_eq!(ran.stderr, "");
        // A line at 1, 2 and 3 s, and one at the end.
        assert_eq!(ran.lines.len(), 4, "{:?}", ran.lines);
        for line in &ran.lines {
            assert_eq!(shape(line), UNREAD);
        }
        server.wait_no_node(NODE, WITHIN);
    }

    #[test]
    fn a_capture_with_no_audio_and_a_missing_server_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(dir.path());
        // With no server at all, a refusal of the file shows it is read before any connection or node.
        server.stop();
        let record = |direction, sdu| Record {
            at_ns: 1,
            direction,
            sdu,
        };
        let audio = audio_sdu(0, &[tone_unit(LINE, LEVEL)]);
        let cut = audio[..AUDIO_HEADER_LEN - 1].to_vec();
        // Audio the host sent is not the pods' microphone, and a packet cut short holds none.
        let sent = [
            record(Direction::ToPods, START.to_vec()),
            record(Direction::ToPods, audio),
            record(Direction::FromPods, cut),
        ];
        put_file(dir.path(), "no-audio.log", log_text(&sent).as_bytes());
        let no_audio = dir.path().join("no-audio.log");
        let ran = Replay::start(&server, dir.path(), &no_audio).finish();
        assert_eq!(ran.status.code(), FAILED, "{}", ran.stderr);
        assert!(ran.lines.is_empty(), "{:?}", ran.lines);
        let said = [
            "packets from the pods cut short before their fixed fields, left out: 1",
            "holds no audio packet",
        ];
        for words in said {
            assert!(ran.stderr.contains(words), "{}", ran.stderr);
        }

        let capture = kept(dir.path(), &capture(SHORT_PACKETS, on_time));
        let ran = Replay::start(&server, dir.path(), &capture).finish();
        assert_eq!(ran.status.code(), FAILED, "{}", ran.stderr);
        assert!(ran.lines.is_empty(), "{:?}", ran.lines);
        assert!(ran.stderr.contains("no audio server"), "{}", ran.stderr);
    }
}
