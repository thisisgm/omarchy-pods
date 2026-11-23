# Service harness

`shell.qml` is a Quickshell configuration that runs the plugin's real
`Service.qml` in the real Quickshell, against a state directory the run made,
and prints one line for every change of `daemonReachable`, `connected`,
`lastError`, `actionStatus`, `schemaUnsupported` and `noiseMode`:

```
harness {"daemonReachable":true,"connected":false,"lastError":"","actionStatus":"","schemaUnsupported":false,"noiseMode":-1}
```

The first line is the state before the first read. Quickshell prefixes each
line with its log level, so match on `harness {`.

Quickshell resolves imports only inside its config folder, and the stock
plugin validator refuses a plugin folder that holds a symlink, so each run
copies `shell.qml`, `Service.qml` and `Model.js` into a folder of its own and
runs Quickshell on that folder.

| Variable | Meaning |
|---|---|
| `XDG_STATE_HOME` | Required, absolute. The harness refuses to start without it, so the real state directory is never read. |
| `OMAPODS_HARNESS_SECONDS` | How long the harness runs before it ends itself. Default 10. |
| `OMAPODS_HARNESS_CHANGE` | The key `noise` and a mode as `librepods set` takes them, such as `noise anc`. The harness calls `setNoiseMode` with that mode once, one second after start, and prints `harness sent noise anc`. Refused unless `XDG_RUNTIME_DIR` sits beside `XDG_STATE_HOME` in the run's directory, because with the session's own runtime directory and a client on `PATH` the change would reach real AirPods. |

Exit status 0 is a run that ended on its timer, 2 is a refusal, and its
reason is printed as `harness refused: ...`.

## Requirements

A desktop session: Quickshell needs a display even though the harness draws
nothing. Run it from the repository root, in a terminal of that session. The
sandbox gives the run a runtime directory of its own and names the session's
display by its full path, so Quickshell's run record stays inside the run and
a client started by `Service.qml` cannot find a running daemon's socket.

## Setup, shared by every arm

Every arm runs in a directory of its own. Nothing outside it is written.

```bash
run=$(mktemp -d)
mkdir -p "$run/state/librepods" "$run/home" "$run/config"
mkdir -m 700 "$run/runtime"
cp tests/harness/shell.qml Service.qml Model.js "$run/config/"
state="$run/state/librepods/status.json"
sandbox=(env HOME="$run/home" XDG_STATE_HOME="$run/state"
  XDG_CONFIG_HOME="$run/home/.config" XDG_CACHE_HOME="$run/home/.cache"
  XDG_DATA_HOME="$run/home/.local/share"
  XDG_RUNTIME_DIR="$run/runtime" WAYLAND_DISPLAY="$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY")
away='{"connected":false,"noise_mode":-1,"schema_version":1}'
near='{"connected":true,"noise_mode":2,"schema_version":1,"supports_noise_control":true}'
# The same with advert_intake, the key only the Rust daemon writes.
current='{"advert_intake":"paused","connected":true,"noise_mode":2,"schema_version":1,"supports_noise_control":true}'
# Stubs that record their arguments, one run per line, in $run/<name>.log, and exit 0; each arm removes the logs before every run.
mkdir "$run/bin"
for name in librepods librepods-ctl; do
  printf '#!/bin/sh\necho "$*" >> "%s/%s.log"\n' "$run" "$name" > "$run/bin/$name"
  chmod +x "$run/bin/$name"
done
# Replaces the file whole, as both daemons do.
publish() { printf '%s\n' "$1" > "$state.new" && mv -f "$state.new" "$state"; }
# Waits until the output holds at least $2 lines holding $1, for at most 50 tries of 0.2 s.
await() {
  local tries=50
  until [ "$(grep -c -F -- "$1" "$run/out")" -ge "$2" ]; do
    tries=$((tries - 1))
    [ "$tries" -gt 0 ] || { echo "no line holding $1 after 10 s" >&2; return 1; }
    sleep 0.2
  done
}
```

The released `Service.qml` for a negative control:

```bash
mkdir -p "$run/old"
git show v1.3.6:Service.qml > "$run/old/Service.qml"
git show v1.3.6:Model.js > "$run/old/Model.js"
cp tests/harness/shell.qml "$run/old/"
```

and `-p "$run/old"` in place of `-p "$run/config"` below.

When the arm is done:

```bash
case "$run" in /tmp/*) rm -r -- "$run" ;; esac
```

## an_oversized_file_is_refused_and_the_shell_does_not_grow

```bash
publish "$away"
"${sandbox[@]}" OMAPODS_HARNESS_SECONDS=20 qs --no-color -p "$run/config" > "$run/out" 2>&1 &
pid=$!
await '"daemonReachable":true' 1
grep VmRSS "/proc/$pid/status"
head -c 67108864 /dev/zero | tr '\0' x > "$state.new" && mv -f "$state.new" "$state"
await 'status file' 1
grep VmRSS "/proc/$pid/status"
wait "$pid"
```

Passes when the last `harness` line carries
`"lastError":"The librepods status file is too large to read"` and the second
`VmRSS` is within a few MiB of the first, far under the 64 MiB file. The
released `Service.qml` reads the whole file instead: its second `VmRSS` is
larger by at least the file's size, and its line says
`Could not read the librepods status file`.

Run it a second time with the 64 MiB file in place before the harness starts
and compare its `VmRSS` after the first line against a start with `$away`, so
the first read is covered as well as a change.

## a_document_of_the_old_daemon_reads_as_before

With a 1.3.6 daemon running, copy its document into the sandbox:

```bash
cp "${XDG_STATE_HOME:-$HOME/.local/state}/librepods/status.json" "$state"
"${sandbox[@]}" OMAPODS_HARNESS_SECONDS=3 qs --no-color -p "$run/config" > "$run/out" 2>&1
grep -F 'harness {' "$run/out" | tail -n 1
```

Passes when that line reads `"daemonReachable":true`, `"lastError":""`, and
`connected` equal to the document's own `connected`, and when the released
`Service.qml` prints the same line for the same copy.

## a_file_replaced_by_rename_is_read_again

```bash
publish "$away"
"${sandbox[@]}" OMAPODS_HARNESS_SECONDS=20 qs --no-color -p "$run/config" > "$run/out" 2>&1 &
pid=$!
await '"daemonReachable":true' 1
publish "$near";  await '"connected":true' 1
publish "$away";  await '"connected":false' 3
rm -- "$state";   await '"daemonReachable":false' 2
publish "$away";  await '"daemonReachable":true' 4
wait "$pid"
grep -F 'harness {' "$run/out"
```

Passes when every `await` returns 0 and the lines read, in order: not
reachable (before the first read), reachable and not connected, connected,
not connected, not reachable (the file is gone, as when the daemon stops),
and reachable again (the daemon is back).

## A change with no librepods-ctl on PATH

```bash
publish "$near"
"${sandbox[@]}" PATH=/usr/bin OMAPODS_HARNESS_CHANGE="noise anc" OMAPODS_HARNESS_SECONDS=8 \
  qs --no-color -p "$run/config" > "$run/out" 2>&1
grep -F -e 'harness' -e 'failed to start' "$run/out"
```

`PATH=/usr/bin` still holds `qs` and `head`, and not `librepods-ctl`, which
`setup` installs under `~/.local/bin`. This arm records what the panel shows a
person whose client is missing; it is not a pass rule. The head and the
released `Service.qml` print the same: `harness sent noise anc`, then
Quickshell's
`WARN: Process failed to start, likely because the binary could not be found`,
and no further `harness {` line, because Quickshell emits no `exited` for a
process that failed to start, so `actionStatus` stays empty.

## a_current_daemon_gets_set

```bash
publish "$current"
rm -f -- "$run/librepods.log" "$run/librepods-ctl.log"
"${sandbox[@]}" PATH="$run/bin:/usr/bin" OMAPODS_HARNESS_CHANGE="noise anc" OMAPODS_HARNESS_SECONDS=3 \
  qs --no-color -p "$run/config" > "$run/out" 2>&1
grep -F 'harness sent' "$run/out"; cat "$run/librepods.log"; ls "$run/librepods-ctl.log"
```

Passes when `librepods.log` holds the one line `set noise anc` and
`librepods-ctl.log` does not exist. The released `Service.qml` runs
`librepods-ctl noise:anc` for the same document, so its `librepods.log` does
not exist and its `librepods-ctl.log` holds `noise:anc`.

## an_old_daemon_gets_the_old_verb_and_never_set

With a 1.3.6 daemon running, copy its document into the sandbox:

```bash
cp "${XDG_STATE_HOME:-$HOME/.local/state}/librepods/status.json" "$state"
rm -f -- "$run/librepods.log" "$run/librepods-ctl.log"
"${sandbox[@]}" PATH="$run/bin:/usr/bin" OMAPODS_HARNESS_CHANGE="noise anc" OMAPODS_HARNESS_SECONDS=3 \
  qs --no-color -p "$run/config" > "$run/out" 2>&1
grep -F 'harness sent' "$run/out"; cat "$run/librepods-ctl.log"; ls "$run/librepods.log"
```

Passes when `librepods-ctl.log` holds the one line `noise:anc` and
`librepods.log` does not exist, and when the released `Service.qml` leaves the
same two files for the same copy.

## a_refusal_reaches_action_status_and_clears_the_hold

The `librepods` stub prints what `librepods set` prints for a bad value and
exits 1:

```bash
cat > "$run/bin/librepods" <<STUB
#!/bin/sh
echo "\$*" >> "$run/librepods.log"
printf 'That setting does not take this value.\\nValues: off, anc, transparency, adaptive, cycle\\n' >&2
exit 1
STUB
publish "$current"
rm -f -- "$run/librepods.log" "$run/librepods-ctl.log"
"${sandbox[@]}" PATH="$run/bin:/usr/bin" OMAPODS_HARNESS_CHANGE="noise anc" OMAPODS_HARNESS_SECONDS=6 \
  qs --no-color -p "$run/config" > "$run/out" 2>&1
grep -F -e 'harness' -e 'Values' "$run/out"; cat "$run/librepods.log"
```

Passes when `librepods.log` holds `set noise anc`, no line of the output holds
`Values`, and the `harness {` lines after `harness sent noise anc` read, in
order: `"noiseMode":1` (the click, held), a line with
`"actionStatus":"That setting does not take this value."`, a line with that
`actionStatus` and `"noiseMode":2` (the hold dropped and the file read again),
then `"actionStatus":""`. The hold lasts 4 s and the status 2.2 s, so
`"noiseMode":2` before `"actionStatus":""` shows the refusal dropped the hold.
The released `Service.qml` sends `librepods-ctl noise:anc` instead, which the
stub takes, so it shows no `actionStatus` at all.

## a_refusal_is_worded_by_the_program_that_ran

The `librepods` stub replaces the document with one of the old daemon while it
runs, waits for the panel to read it, then refuses:

```bash
cat > "$run/bin/librepods" <<STUB
#!/bin/sh
echo "\$*" >> "$run/librepods.log"
printf '%s\n' '$away' > "$state.new" && mv -f "$state.new" "$state"
sleep 1
printf 'That setting does not take this value.\\nValues: off, anc, transparency, adaptive, cycle\\n' >&2
exit 1
STUB
publish "$current"
rm -f -- "$run/librepods.log" "$run/librepods-ctl.log"
"${sandbox[@]}" PATH="$run/bin:/usr/bin" OMAPODS_HARNESS_CHANGE="noise anc" OMAPODS_HARNESS_SECONDS=6 \
  qs --no-color -p "$run/config" > "$run/out" 2>&1
grep -F -e 'harness' -e 'Values' "$run/out"; cat "$run/librepods.log"
```

Passes when `librepods.log` holds `set noise anc`, a `harness {` line with
`"connected":false` comes before the first line with
`"actionStatus":"That setting does not take this value."`, and no line of the
output holds `Values`. The negative control is the same run with
`root.actionStatus = commandProcess.command[0] === Model.SET_PROGRAM` in the
run's copy of `Service.qml` replaced by `root.actionStatus = root.daemonCurrent`:
the panel has read the old daemon's document by the exit, so it words the
refusal as `librepods-ctl`'s, and `actionStatus` holds the `Values` line.
