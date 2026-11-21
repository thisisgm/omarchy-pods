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

### `do`

Asks for one action, named by `action`: `connect`, `disconnect` or `forget`.

## Replies

A reply carries the request's `id` and `ok`.

```
{"id":7,"ok":true,"state":{ ...the state object... }}
{"id":9,"ok":true,"result":"sent"}
{"id":9,"ok":false,"error":{"code":"not_connected","message":"The AirPods are not connected."}}
{"id":3,"ok":false,"error":{"code":"unsupported_version","message":"This daemon speaks protocol version 1 only.","supported":[1]}}
```

- `get` and `subscribe` reply with `state`, the same object `librepods status`
  prints and the state file holds.
- `set` and `do` reply with `result`: `sent` when the command went to the
  AirPods with no answer yet, `confirmed` when it took effect and the state
  shows it.
- A refused request replies with `error`: `code` for programs and `message`, one
  sentence for people.

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
| `not_connected` | The AirPods are not connected. | The command needs the AirPods and they are not connected. |
| `not_supported` | These AirPods do not support that. | The model has no such feature. |
| `not_worn` | The AirPods are not in an ear. | The command needs the AirPods in an ear. |
| `out_of_range` | The value is outside the range the setting takes. | `adaptive_level` outside 0 to 100. |
| `unknown_key` | There is no setting by that name. | `set` with a `key` the table above does not list. |
| `bad_request` | The daemon could not read the request. | See below. |
| `timeout` | The AirPods did not answer in time. | The AirPods did not answer. |
| `headless` | This daemon runs headless and has no window. | Not sent in protocol 1. |
| `unsupported_version` | This daemon speaks protocol version 1 only. | The line has a usable `id`, and `v` is present and is not `1`. The error lists the versions the daemon speaks in `supported`. |

`bad_request` covers:

- a line that is not valid JSON, is not a JSON object, or has no usable `id`,
  which gets `"id":null`;
- a line with a usable `id` but no `v`, an `op` that is missing or not one of
  the four, a `set` without a string `key` and a string `value`, a `value` the
  key does not take, a `do` without a string `action` or with an action not
  listed, each of which echoes the `id`.

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
  exit 0 on success, or print the error message on stderr and exit 1.
- When no daemon answers, each prints `Could not connect to OpenPods daemon at
  <socket> (is it running?)` on stderr and exits 1.
- Each reads lines from the daemon of at most 65536 bytes, newline excluded,
  and takes only the shapes given above: a reply with the request's `id` and
  `ok` true carrying the `state` or `result` its `op` gets; a refusal with `ok`
  false and an `error` holding a string `message`, which is what it prints,
  under the request's `id` or under a null `id` with `bad_request`; for
  `watch`, then state events. Any other line prints
  `The daemon's reply to <command> was not understood.` on stderr and exits 1.
