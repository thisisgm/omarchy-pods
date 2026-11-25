# Protocol 1

The daemon listens on `$XDG_RUNTIME_DIR/librepods.sock`. Protocol 1 is the
daemon's own line protocol on that socket: a client can read the state, change
a setting, ask for an action and follow every change of the state. The
`librepods status`, `watch`, `set` and `do` commands speak it.

This protocol may change until the first release that ships it.

## Choosing the protocol

The first byte a client sends decides what it speaks. A connection that starts
with `{` speaks protocol 1. Any other first byte is the older one-verb dialect
of `librepods-ctl`, which keeps working on the same socket.

A connection that sends no byte within 1 second is closed without a reply. The
daemon serves at most 64 clients at once, in either protocol, and closes the
next one as soon as it connects.

## Lines

Each request and each reply is one JSON object on one line, ended by a newline
(`\n`). A connection can carry any number of requests, and the daemon answers
them in the order they arrive.

- A request line holds at most 4096 bytes, newline excluded. A longer line
  closes the connection without a reply.
- A line nested more than 127 levels deep, objects and arrays counted
  together, is read as not valid JSON.
- Bytes after the last newline wait for the rest of their line. When the client
  ends its side of the connection, the daemon answers the complete lines it has,
  drops an unfinished one and closes.
- The daemon waits on the client while a request line is unfinished, while a
  reply or event it has queued is not yet sent, and, on a connection that has
  not subscribed, at all times. While it waits, it closes the connection after
  2 seconds without progress. The 2 seconds start when the daemon begins to
  wait and start again at each progress: a complete request line answered, or
  a queued line sent in full. The bytes of an unfinished line are not progress.
  A subscriber with nothing unfinished or queued may stay idle as long as it
  likes.
- Members a request does not use are ignored. A member given twice takes its
  last value.

## Requests

Every request has `v`, the protocol version, `id`, a number the reply echoes,
and `op`, what it asks for.

```
{"v":1,"id":7,"op":"get"}
{"v":1,"id":8,"op":"subscribe"}
{"v":1,"id":9,"op":"set","key":"noise","value":"anc"}
{"v":1,"id":10,"op":"do","action":"disconnect"}
```

- `v` is `1`, the only version this daemon speaks.
- `id` is a whole number from 0 to 18446744073709551615, written as plain
  digits. A string, a negative number, a larger number or a number written with
  a decimal point or an exponent (`7.0`, `1e2`) is not a usable `id`.
- `op` is one of `get`, `subscribe`, `set` and `do`.

### `get`

Replies with the current state.

### `subscribe`

Replies with the current state, exactly as `get` does, then sends one event
line for every change of the state after it, in order. The first event is the
first change after the state in the reply, so no change is missed and none is
sent twice. Subscribing again on the same connection replies with the state
again and keeps the one subscription.

### `set`

Changes one setting. `key` names it and `value` is always a JSON string, the
level included.

| `key` | `value` |
|---|---|
| `noise` | `off`, `anc`, `transparency`, `adaptive` or `cycle` (the next mode after the current one) |
| `ear_detection` | `off`, `one` (pause when either bud comes out) or `both` (pause when both are out) |
| `conversational_awareness` | `true` or `false` |
| `one_bud_anc` | `true` or `false` |
| `adaptive_level` | a whole number from 0 to 100, written in decimal, with an optional leading `+` or `-` and optional trailing ASCII spaces, tabs or line breaks |
| `notifications` | `true` (desktop notices on) or `false` (no desktop notice is sent) |
| `microphone` | `auto`, `always` or `off`: whether the daemon may use the AirPods' microphone. With `auto` or `always`, the daemon tries to arm dictation while the link is up. `always` starts the AirPods' microphone uplink while the link is up, the model number is known and a bud is worn. Under the same conditions, `auto` starts it for a recorder or a client asking for fast capture, and probes models whose support is still unknown after trying to arm dictation. `off` removes the microphone source and closes the dictation channel at once. It stops the uplink once no bud is worn or the AirPods' sink is suspended or gone. Stopping the daemon does the same and waits at most 2 seconds for the uplink stop and restore, leaving the uplink running when a worn bud's sink is not known to be suspended or gone by then. Any other end of the want while the link holds lets the uplink linger first, as the `mic` reply says. While a headset profile is active, the daemon waits for the profile to change before sending START or STOP and skips restores. |

`ear_detection`, `notifications` and `microphone` are kept across restarts in
`settings.json` beside the state file, readable only by its owner. When that
file is absent, the daemon takes the first two choices once from the older
`${XDG_CONFIG_HOME:-$HOME/.config}/AirPodsTrayApp/AirPodsTrayApp.conf` and
writes `settings.json`; it never changes the older file. A `settings.json`
without `microphone` reads `auto`. `settings.json` also keeps `mic_models`, the
model numbers whose uplink answered or whose dictation service armed. It keeps
the oldest first, with at most 16 model numbers, each 1 to 8 characters of
`A` to `Z` and `0` to `9`. A seventeenth drops the oldest, and a
file without the key reads an empty list. A `settings.json` the daemon cannot read or
parse, or finds larger than 65536 bytes, gives the defaults, `one`, `true` and
`auto`, and one line in the log; the next change replaces it unless it is a
directory or a symbolic link, which stay in place. `notifications` and
`microphone` have no verb in the older dialect.

### `do`

Asks for one action, named by `action`: `connect`, `disconnect` or `forget`.
Each acts on the AirPods that have the link, else the AirPods that last had it
since the daemon started, else the only AirPods BlueZ knows. When there are
none, or several and none has had the link, it is refused with `no_device`.

- `connect` replies `confirmed` when the link is already up. When BlueZ holds
  the AirPods connected and the daemon waits to dial them again, it dials them
  at once. Otherwise it asks BlueZ to connect them.
- `disconnect` asks BlueZ to disconnect the AirPods, and the state is cleared
  as soon as the link ends. It is refused with `not_connected` unless the
  daemon is linked to them, dialling them or waiting to dial them again, and
  BlueZ reports them connected.
- `forget` asks BlueZ to remove the AirPods, which unpairs them.

Each replies `sent` once the action is handed on, before BlueZ or the AirPods
answer, so `sent` does not mean BlueZ accepted the call. The state then shows
what BlueZ did. A call fails when BlueZ refuses it or does not answer within 30
seconds. It is not made at all when four calls already wait for BlueZ, when
BlueZ no longer holds the AirPods, or when the system bus is out of reach. A
`connect` or `disconnect` that fails or is not made raises
`connect_failures_total` or `disconnect_failures_total` and changes nothing
else; a `forget` that fails or is not made is only logged. While a
`disconnect` waits for BlueZ, the daemon does not dial the AirPods again, and
if BlueZ reports them gone their state is cleared at once; once it fails, the
daemon dials them again as before.

## Replies

A reply carries the request's `id` and `ok`.

```
{"id":7,"ok":true,"state":{ ...the state object... }}
{"id":9,"ok":true,"result":"sent"}
{"id":9,"ok":false,"error":{"code":"not_connected","message":"The AirPods are not connected."}}
{"id":3,"ok":false,"error":{"code":"unsupported_version","message":"This daemon speaks protocol version 1 only.","supported":[1]}}
```

- `get` and `subscribe` reply with `state`, the same object `librepods status`
  prints and the state file holds. Besides the keys the older daemon wrote, it
  carries `notifications_enabled`, the `notifications` setting, and
  `microphone`, the `microphone` setting. With the setting `off`, `mic` reads
  `stopping` while the link is up and the uplink runs, its STOP is owed or
  awaiting confirmation, or a sink restore is in progress. Otherwise it reads
  `off`. With the setting `auto` or `always`, `mic` reads `blocked` while the
  link is up and the AirPods card's active profile starts with `headset-`.
  Then it checks for `library_error` once the audio library failed, until the
  daemon restarts; `idle` while no link is up; `unsupported` once a model never
  confirmed left its START unanswered three times; and `failed` once a
  confirmed one did. Both START failure statuses last until the link is lost.
  It reads `stopping` from a STOP until the sink's restore
  after it is done or owed, or, for a STOP sent because the sink was suspended
  or gone, until the AirPods say the uplink stopped; `not_worn` while no bud is
  worn; `starting` from the START until the AirPods say the uplink runs, then
  `recording` while a program records from the AirPods' microphone and `live`
  otherwise; with the uplink stopped, `ready` while the microphone's source is
  on the audio server, `no_audio_server` while the source is wanted and the
  audio server is absent or refuses it, and `idle` otherwise. An uplink no longer wanted while
  the link holds, other than by `off` or a stop of the daemon, lingers: it runs
  on for a minute, or a few seconds once the last bud is out. Its STOP then
  waits, with `mic` still `live`, while a bud is worn and the AirPods' sink is
  not known to be suspended or gone, and goes out once the sink is, or once the
  last bud has been out those few seconds. `off` and a stop of the daemon skip
  the linger: their STOP goes out at once when no bud is worn or the sink is
  suspended or gone, and waits the same way otherwise. Once a STOP is waiting,
  a suspended or absent sink releases it even during the last bud's grace.
  After a STOP the daemon suspends and resumes the AirPods' sink to reopen a
  stream the uplink's end broke. A STOP sent for a suspended or absent sink
  begins with no restore owed. If the sink becomes active or its state becomes
  unknown before confirmation, a restore is owed. A channel drop can still
  leave the sink up and a restore owed. The daemon skips the restore for a sink
  that is already suspended or absent. With no audio server it owes the restore
  until the server is back, and a newer START drops a restore not yet begun.
  At a stop of the daemon the STOP has 2 s to go out, be answered and have its
  restore done; the daemon then exits, and logs that the uplink still runs
  when a worn bud's sink was not known to be suspended or gone in that time.
  A START the AirPods leave unanswered is sent again, three tries in all,
  unless the want or the link is gone or the daemon is stopping, and no STOP
  is sent while one waits for its answer. An uplink the AirPods say
  runs while nothing wants it, such as a late answer, is stopped as one whose
  linger has passed. A STOP the AirPods do not confirm in time is taken as
  done, and a later START still goes out. An uplink the AirPods stop on their
  own gets the restore at once, and starts again only once the want has gone
  and come back. While a headset profile is active, the source is withdrawn
  even with a recorder linked. The daemon waits for the profile to change
  before sending START or STOP and skips restores while blocked, including one
  owed before the block, and drops them rather than running them after the
  profile changes. It only reads the profile.
  At the edge into sleep a running uplink is stopped with no restore when no
  bud is worn or the sink is suspended or gone, and is left
  running otherwise, as the sleep ends the link; a START waiting for its answer
  is dropped; after the wake a stopped uplink starts again while it is wanted.
  Its `mic_start_failures` counts every START left unanswered, each try once,
  since the daemon started.
  `mic_dictation` reads `off` when the link is down or the microphone setting
  is `off`, `arming` while the daemon prepares the service or retries its
  channel, `armed` when subscriptions and codec selection are ready, or
  `unavailable` for the rest of a connection after a terminal failure. The
  daemon prepares the service without setting dictation mode or starting a
  stream. `mic_source` is the name of the daemon's microphone source,
  `omarchypods_input.` and the model number in lower case, while the source is
  on the audio server, and empty otherwise. The source is wanted while the
  link is up, the model has answered an uplink START or armed for dictation,
  the setting is enabled, no headset profile is active, and a bud is worn or
  was taken out less than a few seconds ago. It stays while a program records
  from it, even if the bud is taken out, unless a headset profile is active. It
  carries the AirPods' microphone,
  held a fixed delay behind the audio as it arrives, and silence while none
  comes. A program that records from it wants the uplink as `always` does,
  while a bud is worn. A client with `session.fast-capture-start=true` also
  wants it before any recorder links. When that client leaves, the uplink
  follows the same linger and playback gate if no other client or recorder
  wants it. Loss of the audio server clears the hint and the headset block,
  and the source retries while still wanted. A wanted running uplink that
  sends no audio for about a second while a bud is worn is sent START again,
  unless the system is going to sleep or the daemon is stopping.
  Each such stall counts as an unanswered try in `mic_start_failures`. Once
  the tries are spent the uplink is stopped and `mic` reads `failed`.
  Its `mic_underruns` counts the times the source ran out of audio under a
  recorder, `mic_concealed` the units of audio written
  in place of audio lost or refused, and `mic_decode_errors` the units the
  decoder refused or that came past the eight one packet may carry, each since
  the daemon started and updated only when `mic` changes or a recorder leaves.
  The source applies a provisional +14 dB gain, checked with a steady-tone
  fixture. Speech peaks are expected to stay at or below the limiter's knee,
  and the gain still awaits measurement on speech. Above the knee, the
  limiter bends samples to keep them below its ceiling. `mic_limited` counts
  samples bent by the limiter. `mic_nonfinite` counts samples that were
  not finite (NaN or infinite) and went out as silence. Both counters follow
  the same rules as the other three.
  A unit is one frame of the AirPods' microphone codec, 480 samples at 64 kHz,
  7.5 ms of audio. Its `audio_server` is `idle` while no AirPods
  link is up, `up` while the daemon reads the desktop's audio server, and
  `down` while it cannot reach it. Its
  `advert_intake` is `listening` once BlueZ accepted the daemon's advert
  monitor, which reads the AirPods' battery, case, lid and ear state from their
  adverts while no link is up; `unavailable` while the monitor is wanted,
  because the AirPods' keys are kept and no link is up, and BlueZ has not
  accepted it because no adapter offered advert monitors or BlueZ refused each
  try; and `paused` otherwise, while the link is up, the AirPods' keys are not
  kept or BlueZ has not accepted the registration yet.
- `set` and `do` reply with `result`. `confirmed` means the state shows the
  change and nothing had to be sent: `ear_detection`, `microphone`, and
  `connect` with the link up. `sent` means the command was handed on and the
  reply does not wait for its end. A setting of the AirPods that is not
  refused is written on every ask while the link is up, even when the state
  already shows the value, and is answered `sent` once the write is on the
  link; the state shows the
  value written at once. The AirPods may report a queued write late, so for
  3 s after the last write of a setting, a report of it with another value is
  held back until they report the value last written; if the 3 s pass first,
  the last report held back is taken then. After a `do` the state shows what
  BlueZ did, and a call that fails
  or is not made shows only in a counter or the log, as described under `do`.
- A refused request replies with `error`: `code` for programs and `message`, one
  sentence for people.
- A refused `set` may still have stored its value and counted the change: the
  state and its event are the truth, and the AirPods' own report replaces the
  value when they connect.

## Events

After `subscribe`, the daemon sends one line per change of the state:

```
{"event":"state","state":{ ...the state object... }}
```

The daemon keeps at most 16 lines waiting for a client that reads slowly. When
a 17th would wait, or 2 seconds pass while lines wait and none of them is sent
in full, it closes that connection rather than skip a change. A subscriber
therefore sees every change in order, or its connection closes.

## Errors

| `code` | `message` | When |
|---|---|---|
| `not_connected` | The AirPods are not connected. | The command needs the AirPods and they are not connected, its write to them failed, or `disconnect` found them not linked, dialled or waiting to be dialled, or not connected in BlueZ, as described under `do`. |
| `not_supported` | These AirPods do not support that. | The model has no such feature or listening mode, or `adaptive_level` was set while the listening mode is not adaptive. |
| `not_worn` | The AirPods are not in an ear. | A listening mode change while no bud is in an ear, which the AirPods do not answer, so nothing is written. Over ear AirPods are always written to. |
| `out_of_range` | The value is outside the range the setting takes. | `adaptive_level` outside 0 to 100. |
| `bad_value` | That setting does not take this value. | `set` with a `value` its `key` does not take, as `loud` for `noise` or `high` for `adaptive_level`. |
| `unknown_key` | There is no setting by that name. | `set` with a `key` the table above does not list. |
| `bad_request` | The daemon could not read the request. | See below. |
| `timeout` | The AirPods did not answer in time. | The daemon's input queue, which the AirPods' packets share, was full. |
| `headless` | This daemon runs headless and has no window. | Not sent in protocol 1. |
| `no_device` | There are no AirPods to act on. | A `do` found no AirPods to act on, as described under `do`. |
| `unsupported_version` | This daemon speaks protocol version 1 only. | The line has a usable `id`, and `v` is present and is not `1`. The error lists the versions the daemon speaks in `supported`. |

`bad_request` covers:

- a line that is not valid JSON, is not a JSON object, or has no usable `id`,
  which gets `"id":null`;
- a line with a usable `id` but no `v`, an `op` that is missing or not one of
  the four, a `set` without a string `key` and a string `value`, a `do`
  without a string `action` or with an action not listed, each of which echoes
  the `id`.

Every reply to a line with a usable `id` echoes that `id`, refusals included.
The daemon checks a line in this order and replies to the first rule it breaks:
a usable `id`, then `v` present, then `v` is `1`, then the rest. A line with a
usable `id` whose `v` is present and is not `1` therefore gets
`unsupported_version` whatever else it holds, and a line with no usable `id`
gets `bad_request` whatever its `v`.

The connection stays open after any error reply.

## Command line

- `librepods status` prints the state line.
- `librepods watch` prints the state line, then one line per change. When the
  connection ends, because the daemon stopped or closed it for reading too
  slowly (see Events), it prints `The daemon went away.` on stderr and exits 1.
  When nothing reads its output any more, it notices at the next change, when
  that line cannot be written, and exits 0, so `librepods watch | head -1`
  succeeds but returns only after the next change of the state.
- `librepods set <key> <value>` and `librepods do <action>` print nothing and
  exit 0 on success, or print the error message on stderr and exit 1. After
  `bad_value`, `set` also prints `Values:` and the words the key takes, as
  `Values: off, one, both` for `ear_detection`; `adaptive_level` takes a
  number and gets no such line.
- When no daemon answers, each prints `Could not connect to OpenPods daemon at
  <socket> (is it running?)` on stderr and exits 1.
- Each reads lines from the daemon of at most 65536 bytes, newline excluded,
  and takes only the shapes given above: a reply with the request's `id` and
  `ok` true carrying the `state` or `result` its `op` gets; a refusal with `ok`
  false and an `error` holding a string `message`, which is what it prints,
  under the request's `id` or under a null `id` with `bad_request`; for
  `watch`, then state events. Any other line prints
  `The daemon's reply to <command> was not understood.` on stderr and exits 1.
