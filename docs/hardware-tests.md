# Hardware tests

`cargo xtask hw link` runs the daemon's link checks against real AirPods. The
Rust test suite covers everything that can be driven without a radio; these
checks cover what cannot: adopting a link BlueZ already holds, reconnecting,
idle wakeups, and whether each command is written and shown at once.

A person starts every run by hand, on a commit of this repository itself. No
runner is registered for it, so code from a fork never reaches the machine that
holds the AirPods.

## What it needs

- The machine with the AirPods paired and connected, in a graphical session
  with BlueZ, PipeWire and the installed `librepods.service`.
- A release build of `librepods` and of `xtask` copied to a private temporary
  directory on that machine:

  ```bash
  cargo build -p librepods -p xtask --release --locked
  ```

- The installed `librepods-ctl` under `~/.local/bin`, which check C3 compares
  against and check C9 reads the installed daemon's line with.

Then, on that machine:

```bash
./xtask hw link --binary ./librepods                 # unattended checks
./xtask hw link --binary ./librepods --attended      # also the command census
./xtask hw link --binary ./librepods --attended --adapter-cycle
```

`--attended` means a person is at the machine: the census ends by asking for
both buds to be taken out and Enter pressed on standard input, within 5
minutes. `--adapter-cycle` needs `--attended`, because it powers the adapter
off and on and every Bluetooth device on the machine drops once.

## What it stops and what it changes

1. It takes an exclusive lock on `librepods-hw.lock` in
   `$XDG_RUNTIME_DIR` and holds it until after the restore; a second run
   started meanwhile ends at once, having stopped nothing. It also refuses
   to start if a user unit named `librepods-hw` exists, which means an
   earlier run left a failed unit
   (`systemctl --user reset-failed librepods-hw` clears that).
2. It notes whether `librepods.service` is running and stops it. From that
   moment the installed service is restored on every way out: success, a
   failed check, a panic, SIGINT, SIGTERM, SIGHUP, or a tool the run needs
   that fails or hangs and is killed at its timeout.
3. It starts the binary under test as the transient user unit `librepods-hw`,
   with every hardening directive of `packaging/systemd/librepods.service`
   from `UMask` on, except the ones naming the installed daemon's own
   directories. Its runtime and state directories are a private temporary
   directory, so the run never touches the real socket or state file. It uses
   the real system bus and the real session bus.
4. After the checks it stops `librepods-hw`, runs C8 with no daemon
   running, and starts `librepods.service` again if it was running before.

Every program it runs is waited for before it exits, or killed at its
timeout. SIGKILL cannot be caught, so after one nothing is restored: run
`systemctl --user stop librepods-hw` and `systemctl --user start
librepods.service` by hand.

It stops no other unit. It never forgets, pairs or trusts a device, never runs
a card profile command, and puts back every setting the census changes,
reporting any it could not. The adapter is powered back on whatever ends the adapter check.

Expect the panel to show nothing for the whole run (about 30 minutes, longer
with `--attended`), and the AirPods audio to stop for a few seconds at each of
the 5 reconnects of C4, for the whole window of C5 with nothing connected, and
once more in the adapter check with `--adapter-cycle`.

## The checks

| Check | What it does | Passes when |
|---|---|---|
| C1 adoption | restarts the unit 10 times with the AirPods connected | each restart reaches a populated state |
| C2 stop | stops the unit with the link up | the unit is inactive within 1 s, the socket and state file are gone, and BlueZ still holds the device |
| C3 same contract | reads `librepods status` and the installed `librepods-ctl status` | the keys are exactly the status document's, and both print the same line |
| C9 old line | reads the status line of the installed daemon before it is stopped, then the Rust daemon's on the same pods | every one of the C++ daemon's 36 keys is present in both (the Rust daemon's line adds 3, for 39), and each key outside the list below has byte equal values |
| C4 reconnect | `do disconnect` then `do connect`, 5 times | `connected` is false within 5 s and the state is populated again within 15 s |
| C5 idle | 10 minutes with the link up, then 10 minutes after `do disconnect` | the daemon's wakeups stay within the bounds below and the adapter is never discovering |
| C6 commands | each command of the census 10 times, then one with no bud in (`--attended`) | every command answers `sent` 10 of 10 and the status shows its value right after each reply, the settings put back read back equal to the ones found, and the last answers `not_worn` |
| C7 adapter | `bluetoothctl power off`, `power on`, `do connect` (`--adapter-cycle`) | `connected` is true before and false after power off, and the state is populated again |
| C8 burst | opens the AirPods' control channel once, runs the opening exchange and reads for 3 s after the notification request | the distinct opcodes and the distinct setting identifiers equal those of the sample burst the test suite's fake AirPods play, and every packet parses |

C9 runs when the installed daemon is the C++ one, which its line shows by
carrying none of the three keys the Rust daemon adds. Otherwise, or when
`librepods.service` was not running, it reports `skipped` with the reason,
which does not fail the run. It prints each key with `equal`, `differs`,
`only new` or `only old`, and the byte length of each line, never a value.
The keys allowed to differ are the `*_total` counters, which count each
daemon's own calls, the three added keys, and `left`, `right`, `case`,
`headset` and `lid_state`, which follow the AirPods between the two reads.
`device_name` must hold the same bytes in both, so when the pods' name holds
U+2019 the check shows both daemons write it the same way; a name without it
tests nothing about that mark. Both lines are kept only as files of mode 0600
in the run's private directory, which the run removes when it ends.

C8 runs on every invocation, last, after `librepods-hw` has stopped and
before `librepods.service` is started again, so only one program holds a
control channel to the AirPods at a time. It asks BlueZ for the AirPods it
reports connected and opens the channel to that device only; with none it
fails as `no connected AirPods` and opens no socket. It prints one line of
counts, such as `36 packets; opcodes 04 x1, 06 x1, 09 x22, ...; settings 01
to 16; unparsed none`, and on a failure the missing and extra opcodes and
setting identifiers, the opcodes of packets that did not parse and the
number of packets with no opcode, cut short or without the accessory
header. A packet that arrived and did not parse counts under its opcode and
under `unparsed`, so a missing opcode is one the AirPods never sent. It
keeps no payload, so no name, serial number or address can reach its
output. A difference is a fact about the AirPods: the sample is corrected to
match it, never the check loosened.

C6 sends each command over protocol 1 on the test socket rather than through
`librepods set`, which prints nothing when a command succeeds and so cannot
show the reply. One 10 s bound covers each command's connect, request and
reply. The first command that gets no reply at all within it ends the census
as failed and is named, and putting the settings back stops at its first
unanswered command too; a `timeout` reply is an answer.
Each key's rows alternate, so a status that stays at one value fails the row
asking for the other.
C6 needs AirPods whose status shows noise control, adaptive mode,
conversational awareness and one bud noise control; with any of them missing
it fails, naming what is missing, and sets nothing.

Before the checks it prints whether a Bluetooth audio card sits in a headset
profile, because a connect can time out in that state.

Timings are printed as a range with the number of trials, never as one number:

| Gate | What | Bound |
|---|---|---|
| G1a | unit start to populated, from the journal's own stamps | at most 2 s in 10 of 10 |
| G1b | `link: idle to opening` to populated, after `do connect` | at most 500 ms in 4 of 5; a miss prints its phase lines |
| G2b | wall time of each census command | reported only, no bound yet |
| G3 | voluntary context switches of the daemon, a minute | at most 12 with the link up, at most 2 with nothing connected, printed beside the number of `librepods watch` lines in the same window |
| G4b | `do connect` reply to populated | at most 15 s in 5 of 5 |
| G4c | adapter power on to populated | reported only, no bound yet |

The output carries key names, booleans, counts and times. It never prints an
address, a device name or a serial number, and never echoes what a tool wrote
on its standard error.

## Exit status

| Status | Meaning |
|---|---|
| 0 | every check passed and every bound was met |
| 1 | a check failed or a bound was missed, the run was stopped by a signal, or a tool the run needs failed; `pactl` failing only prints the headset profile line as `unknown` |
| 2 | wrong usage, another run holds the lock, the lock in `$XDG_RUNTIME_DIR` could not be opened or taken, or a `librepods-hw` unit already exists; nothing was stopped |
| 77 | the AirPods did not connect within 20 s, so nothing was measured; the service was restored |
| 70 | the installed service could not be restored; this is printed last |
| 101 | the run panicked; the restore ran as it unwound, and its line follows the panic message |

## The uplink sitting

Four step files under `crates/xtask/steps/` drive the microphone uplink of real
AirPods through `xtask probe capture --steps`. They answer what only a person
wearing the pods can say: whether a stop silences a bud, what brings it back,
and what the pods answer to a start or a stop out of turn. Two people sit: one
at the keyboard, one wearing both buds with music playing on them. Nobody
starts a run without the wearer present.

A step file is read only when it is the caller's alone, so each is copied
beside the built `xtask` with `install -m 600`. Every run goes into the store
of `xtask probe init`, on the route the step files run on:

```bash
./xtask probe capture --stamp <stamp> --run <name> --route dictation --steps ./revive
```

A cue is printed as `cue <text>` and the run waits there for Enter, with no
timeout. What was typed before a cue was shown does not answer it. The Enter
that answers a cue is stamped in `marks.tsv` as an `answer` mark, and the waits
after the cue count from it. A run whose
standard input has ended stops at its first cue, and so does Ctrl+C at any
moment; both send the stop of the uplink before the run ends. The person at
the keyboard asks each question aloud and writes the answer in the sitting's
notes. A cue that begins `controller:` asks for a command typed by hand in a
second terminal, then Enter.

| File | Runs | What is written down |
|---|---|---|
| `revive` | three | Whether one bud is silent after the stop, and whether the second start alone, with no restore of the sink before it, brings the sound back in both buds. |
| `restore` | one | At each of its eight cues one restore form is run on the pods' sink, each form twice, in an order written down before the sitting. After each: did the wearer hear a gap, and are both buds playing. |
| `stop-early` | one | A stop written before the pods answered the start: which markers the pods then send and in which order, whether audio packets follow, and whether a bud is silent. |
| `start-twice` | two | A start while the uplink already runs: whether a second running marker comes, whether the audio stamps break, and whether the one stop ends the uplink: do stopped markers come, and do audio packets end. |

Whether one stop ends an uplink started twice is not known. The summary of that
run says `STOP was sent` because the stop left, not because the uplink ended.
If audio packets still come after the stop, run `./xtask probe stop --route
uplink` before anything else.

A step file refuses control commands, opcode 0x0009, unless the run is given
`--allow-control`; the run cannot put a setting back, so a setting a step
changes lasts until the pods reconnect. An uplink start with another codec id
goes out under the run's own uplink guard, so the run still sends the stop.

The restore forms, with `<sink>` the pods' sink in `pactl list sinks short`:

```bash
pactl suspend-sink <sink> 1; pactl suspend-sink <sink> 0
pactl suspend-sink <sink> 1; sleep 0.1; pactl suspend-sink <sink> 0
pactl suspend-sink <sink> 1; sleep 0.3; pactl suspend-sink <sink> 0
pactl suspend-sink <sink> 1
```

The time each form held the sink is read from the suspend and the start of the
audio stream in a Bluetooth trace of the sitting, not from the clock of the
hand. The shortest form nobody heard in two runs of two is the one to keep.
The last form says whether the resume does anything, or the sink comes back by
itself.

Also written down in the same sitting:

- **The first burst.** The pods send one packet of opcode 0x0e when the channel
  opens. Its length and its last byte are read from any run's `sdu.log`, and
  nothing else of it is printed, because the bytes between identify a device.
  The state marker has the same opcode and comes later in a run that starts the
  uplink, so only the first match is read:

  ```bash
  grep -m 1 ' < 040004000e00' sdu.log | while read -r _ _ hex; do
      echo "$(( ${#hex} / 2 )) bytes, last byte ${hex: -2}"
  done
  ```

- **The idle sink.** With music paused for two seconds, so the sink is idle and
  not yet suspended, `./xtask probe stop --route uplink` and then play at once.
  Are both buds playing? Three tries.
- **The first word.** `--route uplink --script firstword`, three times, with
  the wearer counting aloud from before the run starts, so the loss at a cold
  start is measured with speech under way.
- **Read, changing nothing:** whether any client in
  `pactl -f json list clients` carries `session.fast-capture-start` while a
  dictation program is awake; `wpctl settings
  bluetooth.autoswitch-to-headset-profile`, and whether a saved setting for
  that key exists; whether `omarchy-refresh-config` removes files it did not
  write; and what `pactl list sinks short` shows for the pods' sink while
  paused, each second for eight seconds.

## The microphone source and the lateness run

`cargo xtask probe source --replay <sdu.log>` serves the audio of a capture as
a microphone. It reads the `sdu.log` of a run that `cargo xtask probe capture`
wrote, sends each audio packet through the decode chain at the time the log
holds for it, on the monotonic clock, and offers the result as the node
`omapods_input.probe`, described as `AirPods Microphone (probe)`. So the
jitter a capture recorded is the jitter the node is fed. After the last
packet the node stays for the latency it states, while the reader plays what
it holds; with a recorder linked it stays until three reads in a row have
passed the last sample, which on a test server is enough for the recording to
hold it. When those reads have not come one second later, the command fails
and says the recording may be missing its end. A packet from the pods cut
short before its fixed fields carries no audio; it is left out, and one line
on standard error counts them.

A replay opens no channel to the AirPods, stops no unit and writes no file.
It needs only the session's audio server, whose socket it takes from
`$XDG_RUNTIME_DIR`. The node links to nothing by itself: a recorder names it,
as in `pw-record --target omapods_input.probe out.wav`. The command ends when
the capture ends, or on SIGINT, SIGTERM, SIGHUP or SIGQUIT, and the node goes
with it. It fails, before any node exists, on a file that is not the caller's
own or holds no audio packet, and when no audio server answers.

Without `--replay`, `cargo xtask probe source [--seconds N]` serves the AirPods
of the device file live. It takes the hardware run's lock first and refuses to
start while a hardware run or another live probe holds it. It stops
`librepods.service` if it runs, and starts it again on every way out; the
hardware run's test unit it leaves alone. It dials the control channel, runs the
handshake and sends START; the node is made only once the AirPods answer with
their running marker. Each audio packet then goes through the decode chain as it
lands. With no `--seconds` it runs until a signal. After `N` seconds, or on a
signal, it sends STOP and waits for the stopped marker, then suspends the
device's sink with `pactl suspend-sink`, holds it briefly and resumes it. Both
calls name the sink by its index, since its name holds the device's address, and
the name is never printed. A panic takes the same way out. When START goes
unanswered it sends nothing more and leaves the sink alone. The run fails then,
when STOP goes unanswered (the sink is still restored), when the AirPods stop
the uplink on their own (no STOP is sent, and the sink is still restored), and
when a restore fails.

Every second, and once more at its end, a replay or a live run prints one line
of fixed words and numbers on standard output:

| Field | What it holds |
|---|---|
| `distance` | how far the reader stands from its target, smoothed, in ms; above zero it is ahead; `none` until a recorder first reads |
| `rate` | the rate it asks of the server's converter, in parts per million away from 1; `none` until a recorder first reads |
| `unsteered` | graph cycles in which the server gave it nothing to steer through |
| `underruns` | times the buffer ran dry under a recorder |
| `concealed` | units written in place of audio that was lost or refused |
| `decode errors` | units the decoder refused |
| `decoder resets` | times the decoder was replaced after a fault |

A replay on the machine with the AirPods shows what no test server can:

1. With a recorder linked for the whole of a capture of 10 minutes or more,
   `underruns` must stay 0 and `unsteered` must stay 0. An underrun there is a
   defect, not a figure to note; a rising `unsteered` means the server's
   converter does not follow the rate. Note `distance` and `rate` as ranges.
2. `pw-dump` must show the node's `ProcessLatency` with `ns` set, `quantum` 0
   and `rate` 0, and no port may repeat that figure in a `Latency`.
3. Note what a `pw-record` that names the node does when the command ends
   while it records: whether it ends, hangs or moves to another source.

The lateness run needs the AirPods connected and nobody at the machine. Record
the uplink with `cargo xtask probe capture --script soak --minutes 2` first,
with the AirPods as they lie. If `cargo xtask probe analyse` shows audio
packets for the whole two minutes, record the long run the same way;
otherwise one bud has to be worn for it. The long run is at least 30 minutes
(`--minutes` takes up to 90), with music playing for half of it and the
network in ordinary use.

`cargo xtask probe analyse` then holds, for each start, how late each audio
packet landed: its arrival, less the time of the audio the decode chain has
written up to its end, lost units filled in, less the smallest such figure of
the packets of about one second up to it. Stamps that start again, as a new
run of the stream does, empty that second, as they reset the chain. That is
the lateness the reader's buffer has to cover.

| Row | What it holds |
|---|---|
| `lateness_sdus` | the packets judged |
| `lateness_p99_ms`, `lateness_p99_9_ms`, `lateness_p99_99_ms`, `lateness_max_ms` | the percentiles and the largest |
| `lateness_bin_<n>_ms` | the packets from `n` ms up to 5 ms more; the last bin holds the rest |
| `lateness_over_<n>_ms` | the packets a cover of `n` ms would not have covered |

The largest lateness of every such run decides `JITTER_COVER_MS` in
`crates/mic/src/lib.rs`: the cover is the largest lateness of all the runs on
record plus a margin of 5 ms, rounded up to a multiple of 5 ms. A run whose
lateness gives a larger cover by that rule raises it, in a change of its own,
and the latency the node states follows it.

## The audio bench

No audio quality figure here rests on a listener. `cargo xtask probe bench`
scores audio with tools that live in a cache outside the repository; none is
linked into the daemon. `cargo xtask probe bench setup --cache <dir>` makes the
cache in a directory that does not exist yet, without root, on Arch and on
Debian: it needs `curl`, `sha256sum`, `tar`, a C and a C++ compiler and
`python3` with its `venv` module. A download with another SHA-256 than the one
in `crates/xtask/src/probe/bench/pins.rs` stops the setup and nothing is built
from it. The cache is marked ready last, with its pins and the compiler call of
each library, and `probe bench codec` reads no cache marked otherwise. A setup
that fails leaves its directory behind: remove it, or name another.

| Tool | Pin | How it is installed |
|---|---|---|
| ViSQOL v3, audio and speech mode | `visqol-python` 3.8.0, a pure Python port; it printed the reference implementation's conformance figures to six decimals on the three pairs tried | `pip --require-hashes` from the two requirement files beside the pins, into the cache's own environment |
| libsbc | `sbc-2.2.tar.xz` from kernel.org | one compiler call over the checksummed sources; nothing in the archive is run |
| libfdk-aac | `fdk-aac-2.0.3.tar.gz` | the same |
| Reference music | four lossless stereo recordings from Wikimedia Commons, public domain or CC BY-SA 4.0: jazz funk, piano, harpsichord repertoire, pop rock | fetched whole; the bench cuts 20 s of each and resamples it to 48 kHz |

The scorers of the speech benches are chosen and not installed by any command
yet: `ai-edge-litert` 2.4.0 for ViSQOL's speech mode, `nisqa` 2.0.post2,
`speechmos` 0.0.1.1 for DNSMOS, and `faster-whisper` 1.2.1 with the model
`Systran/faster-whisper-medium.en` at revision
`a29b04bd15381511a9af671baec01072039215e3`, scored with `jiwer` 4.0.0. `nisqa`
pins `torch` 2.2.1, so those run under CPython 3.12.

`cargo xtask probe bench codec --cache <dir> --out <file>` decides the playback
codec. Each excerpt goes through libsbc with the settings PipeWire 1.6.9 gives
SBC-XQ (48 kHz, dual channel, 16 blocks, 8 subbands, loudness, bitpool 39 a
channel, about 504 kbit/s) and through libfdk-aac with its AAC settings (MPEG-2
AAC LC, variable rate mode 5, afterburner, LATM, a peak rate of 376 875 bit/s
from a packet of 1017 bytes), is decoded by the same library, and is scored
against the excerpt with ViSQOL in audio mode.

Those settings are assumed, not read from the pods. PipeWire takes them from
what the sink offers: the bench assumes a sink that offers 48 kHz and a bitpool
of 39 or more for SBC, and for AAC one that allows a variable rate, states
320 000 bit/s or no rate, and takes packets of 1017 bytes, the most that fills
one 3-DH5 packet. A sink that offers less gets a lower bitpool or a lower peak
rate than the bench scores, and the 453 and 256 kbit/s the daemon names for
these profiles are nominal rates, not rates read from a link. The capabilities
the pods state for their media channel settle it, and the table says in its
`settings_` rows which sink it assumed.

The table holds a `visqol_mos_<codec>_<track>` and a
`visqol_vnsim_<codec>_<track>` row for `sbc_xq`, `aac` and `reference`, each the
least and the greatest of `--runs` runs (3 unless given), then the settings and
the version of every tool. A `reference` row is the excerpt against itself,
the most a codec row can reach: the score saturates there, so read the
similarity beside it.

`cargo xtask ci bench` makes a cache, runs the bench in it and fails unless
every codec row lies between 4.5 and its reference row. It downloads and scores
for minutes, so it runs only when named: `cargo xtask ci` alone leaves it out.
