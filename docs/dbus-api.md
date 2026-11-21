# D-Bus interface

The Rust daemon serves one object on the session bus. Its properties mirror the
daemon's state and its methods submit commands, the same commands the control
socket accepts.

The interface may change until the first release that ships it. The `1` in the
interface name is its version, and a change that breaks a client after that
release gets a new name.

| | |
|---|---|
| Bus | session |
| Name | `io.github.thisisgm.Omapods` |
| Object path | `/io/github/thisisgm/Omapods` |
| Interface | `io.github.thisisgm.Omapods1` |

If the session bus is missing, or another process already owns the name, the
daemon logs one line and keeps serving the control socket and the state file
without this interface. The daemon never takes the name from another owner and
never lets another process take it.

## Properties

Every property is read only. Every property except `ApiVersion` emits
`org.freedesktop.DBus.Properties.PropertiesChanged` with its new value. A
signal lists exactly the properties whose value differs from the previous
signal. Changes that land before the daemon sends the signal arrive together
with their latest values, a change undone before then sends nothing, and a
command that changes nothing sends none.

| Property | Type | Value |
|---|---|---|
| `ApiVersion` | `u` | `1`, the version in the interface name. Never changes. |
| `Connected` | `b` | Whether the control link to the pods is up. Battery can keep arriving while this is false. |
| `DeviceName` | `s` | The pods' name, empty when unknown. |
| `ModelName` | `s` | The model's display name, empty when unknown. |
| `NoiseMode` | `s` | `off`, `anc`, `transparency` or `adaptive`. |
| `EarDetection` | `s` | When removing a bud pauses playback: `one` (one bud out), `both` (both out) or `off` (never). |
| `ConversationalAwareness` | `b` | Whether Conversation Awareness is on. |
| `OneBudAnc` | `b` | Whether noise control stays on with one bud in. |
| `AdaptiveLevel` | `y` | The adaptive noise level, 0 to 100. |
| `Batteries` | `a{s(bby)}` | Always the keys `left`, `right`, `case` and `headset`, each `(available, charging, level)`. A part with no reading is `(false, false, 0)`. |
| `InEar` | `a{sb}` | Always the keys `left` and `right`. |
| `Capabilities` | `as` | The names of the status object's true `supports_*` keys without the prefix, sorted, for example `adaptive`. |
| `StatusJson` | `s` | The whole status object, the same JSON the state file holds, without the trailing newline. It changes whenever any key changes, counters included. |

## Methods

Each method returns `s`: `sent` when the pods have the command and have not
answered yet, or `confirmed` when the state already shows it carried out. A
method never waits for the pods. A refused command is an error reply instead.

A `Set` method is the protocol 1 `set` request for the key in the table, as
described in [protocol-1.md](protocol-1.md): it takes the same values and
refuses the same way.

| Method | Argument | Key | Accepts |
|---|---|---|---|
| `SetNoiseMode` | `s` | `noise` | `off`, `anc`, `transparency`, `adaptive`, `cycle` |
| `SetEarDetection` | `s` | `ear_detection` | `off`, `one`, `both` |
| `SetConversationalAwareness` | `b` | `conversational_awareness` | |
| `SetOneBudAnc` | `b` | `one_bud_anc` | |
| `SetAdaptiveLevel` | `y` | `adaptive_level` | 0 to 100 |
| `Connect` | none | | |
| `Disconnect` | none | | |
| `Forget` | none | | |

## Errors

A refused command's error reply carries one of these names. Its message is
the sentence protocol 1 sends with the same code, as listed in
[protocol-1.md](protocol-1.md), so a program reads the name and a person reads
the message.

| Name | Code |
|---|---|
| `io.github.thisisgm.Omapods1.Error.NotConnected` | `not_connected` |
| `io.github.thisisgm.Omapods1.Error.NotSupported` | `not_supported` |
| `io.github.thisisgm.Omapods1.Error.NotWorn` | `not_worn` |
| `io.github.thisisgm.Omapods1.Error.OutOfRange` | `out_of_range` |
| `io.github.thisisgm.Omapods1.Error.UnknownKey` | `unknown_key` |
| `io.github.thisisgm.Omapods1.Error.BadRequest` | `bad_request` |
| `io.github.thisisgm.Omapods1.Error.Timeout` | `timeout` |
| `io.github.thisisgm.Omapods1.Error.Headless` | `headless` |

No method of this interface names a setting, opens a window or waits for the
pods, so none returns `UnknownKey`, `Headless` or `Timeout`. A `Set` method
given a word its key does not take returns `BadRequest`.

A call the bus layer refuses before it reaches the daemon, such as one with the
wrong argument type or one that writes a property, gets a standard
`org.freedesktop.DBus.Error` name instead.

## Examples

Read one property:

```console
$ busctl --user get-property io.github.thisisgm.Omapods /io/github/thisisgm/Omapods io.github.thisisgm.Omapods1 Connected
b false
```

Call one method:

```console
$ busctl --user call io.github.thisisgm.Omapods /io/github/thisisgm/Omapods io.github.thisisgm.Omapods1 SetEarDetection s both
s "confirmed"
```
