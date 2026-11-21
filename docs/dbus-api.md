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
`org.freedesktop.DBus.Properties.PropertiesChanged` with its new value. One
state change sends one signal listing exactly the properties whose value
changed; a command that changes nothing sends none.

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

| Method | Argument | Accepts |
|---|---|---|
| `SetNoiseMode` | `s` | `off`, `anc`, `transparency`, `adaptive` |
| `SetEarDetection` | `s` | `off`, `one`, `both` |
| `SetConversationalAwareness` | `b` | |
| `SetOneBudAnc` | `b` | |
| `SetAdaptiveLevel` | `y` | 0 to 100 |
| `Connect` | none | |
| `Disconnect` | none | |
| `Forget` | none | |

## Errors

An error reply carries one of these names and the error's code as its message.

| Name | Meaning |
|---|---|
| `io.github.thisisgm.Omapods1.Error.NotConnected` | The command needs the pods and they are not connected. |
| `io.github.thisisgm.Omapods1.Error.NotSupported` | The model cannot do this. |
| `io.github.thisisgm.Omapods1.Error.NotWorn` | The command needs the pods in the ears. |
| `io.github.thisisgm.Omapods1.Error.OutOfRange` | A number is outside its range, such as an adaptive level above 100. |
| `io.github.thisisgm.Omapods1.Error.UnknownKey` | A string argument is not one of the accepted values. |
| `io.github.thisisgm.Omapods1.Error.BadRequest` | The request is malformed. |
| `io.github.thisisgm.Omapods1.Error.Timeout` | The pods did not answer in time. |
| `io.github.thisisgm.Omapods1.Error.Headless` | The request needs a window, which the daemon does not have. |

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
