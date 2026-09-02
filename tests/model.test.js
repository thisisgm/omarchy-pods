// Run with: deno run --allow-read tests/model.test.js
// Model.js has no exports, so it is evaluated here rather than imported.

const source = Deno.readTextFileSync(new URL("../Model.js", import.meta.url))
const Model = new Function(
  source + "; return { parseStatus, podFrom, defaultPod, noiseModeVerb, earDetectionVerb, levelFraction, levelText, podMeta, elideError, availableModes, NOISE_OFF, NOISE_ANC, NOISE_TRANSPARENCY, NOISE_ADAPTIVE, LEVEL_UNKNOWN, NOISE_UNKNOWN, EAR_PAUSE_ONE_OUT, LID_UNKNOWN, MAX_ERROR_CHARS }"
)()

let failures = 0

function check(name, actual, expected) {
  const ok = JSON.stringify(actual) === JSON.stringify(expected)
  if (!ok) {
    failures++
    console.log("FAIL " + name + "\n  expected " + JSON.stringify(expected) + "\n  got      " + JSON.stringify(actual))
  }
}

// Byte for byte the line a running daemon wrote, counters and all, copied from the box.
const live = '{"adaptive_level_changes_total":0,"adaptive_noise_level":50,"ca_changes_total":0,"case":{"available":true,"charging":false,"level":100},"connect_calls_total":0,"connect_failures_total":0,"connected":true,"conversational_awareness":true,"device_name":"GM’s AirPods Pro","disconnect_calls_total":0,"disconnect_failures_total":0,"ear_detection_behavior":0,"ear_detection_changes_total":0,"forget_calls_total":0,"is_pro_series":true,"left":{"available":true,"charging":false,"in_ear":false,"level":79},"lid_state":2,"model_int":11,"model_name":"AirPods Pro 3","model_number":"A3064","noise_control_changes_total":0,"noise_mode":1,"one_bud_anc_changes_total":0,"one_bud_anc_mode":true,"reconnect_attempts_total":0,"reconnect_failures_total":0,"reopen_calls_total":0,"right":{"available":true,"charging":true,"in_ear":false,"level":100},"schema_version":1,"supports_noise_off":false}'

const good = Model.parseStatus(live)
check("live line parses", good.ok, true)
check("live modelName", good.modelName, "AirPods Pro 3")
check("live deviceName keeps the daemon's apostrophe", good.deviceName, "GM’s AirPods Pro")
check("live left level", good.left.level, 79)
check("live case has no in_ear", good.caseBattery, { level: 100, charging: false })
check("live supportsNoiseOff is honoured", good.supportsNoiseOff, false)
check("live noiseMode", good.noiseMode, 1)

// A fresh daemon omits left, right and case entirely rather than sending available:false.
const fresh = Model.parseStatus('{"connected":false,"noise_mode":-1,"schema_version":1}')
check("fresh line parses", fresh.ok, true)
check("fresh left is the default pod", fresh.left, Model.defaultPod())
check("fresh case is unknown", fresh.caseBattery.level, Model.LEVEL_UNKNOWN)
check("supports_noise_off absent means the model has Off", fresh.supportsNoiseOff, true)

// available:false means the daemon stopped hearing from the pod, so no flag survives.
const gone = Model.podFrom({ available: false, level: 82, charging: true, in_ear: true })
check("unavailable pod reports no level", gone.level, Model.LEVEL_UNKNOWN)
check("unavailable pod reports no charging", gone.charging, false)
check("unavailable pod reports no in_ear", gone.inEar, false)
check("unavailable pod shows no meta", Model.podMeta(gone), "")

// Every failure path returns the full default shape rather than throwing.
const empty = Model.parseStatus("")
check("empty input is not ok", empty.ok, false)
check("empty input names the failure", empty.lastError !== "", true)
check("empty input still has a left pod", empty.left, Model.defaultPod())

const garbage = Model.parseStatus("not json at all")
check("garbage is not ok", garbage.ok, false)
check("garbage is not schemaTooNew", garbage.schemaTooNew, false)

const noVersion = Model.parseStatus('{"connected":true}')
check("a line with no schema_version is not ok", noVersion.ok, false)

const tooNew = Model.parseStatus('{"schema_version":2,"connected":true}')
check("a newer schema is not ok", tooNew.ok, false)
check("a newer schema is flagged", tooNew.schemaTooNew, true)
check("a newer schema reports both versions", tooNew.lastError, "librepods speaks status schema 2, this panel reads 1")

check("null parses to the default shape", Model.parseStatus("null").ok, false)

// Verbs, and the out-of-range guards that keep a bad index off the command line.
check("noise verb for Adaptive", Model.noiseModeVerb(3), "noise:adaptive")
check("noise verb for an unknown mode is empty", Model.noiseModeVerb(Model.NOISE_UNKNOWN), "")
check("noise verb past the end is empty", Model.noiseModeVerb(4), "")
check("ear verb for never pause", Model.earDetectionVerb(2), "ear:off")
check("ear verb past the end is empty", Model.earDetectionVerb(3), "")

// Meter and label edges.
check("an unknown level draws an empty track", Model.levelFraction(Model.LEVEL_UNKNOWN), 0)
check("a level above 100 is clamped", Model.levelFraction(140), 1)
check("a negative level is clamped", Model.levelFraction(-5), 0)
check("an unknown level reads as dashes", Model.levelText(Model.LEVEL_UNKNOWN), "--")

// Errors are elided to one line rather than dumped.
const long = Model.elideError("x\n\n   y".repeat(60))
check("an elided error is one line", long.indexOf("\n"), -1)
check("an elided error fits the row", long.length <= Model.MAX_ERROR_CHARS, true)
check("elideError copes with nothing", Model.elideError(null), "")

// A Max sends one battery under "headset" and no pods at all, so the panel needs the flag to pick a shape.
const max = Model.parseStatus('{"connected":true,"device_name":"AirPods Max","headset":{"available":true,"charging":false,"level":100},"is_headset":true,"model_name":"AirPods Max","noise_mode":1,"schema_version":1}')
check("max line parses", max.ok, true)
check("max is flagged a headset", max.isHeadset, true)
check("max headset level", max.headset, { level: 100, charging: false })
check("max has no pods", max.left.level, Model.LEVEL_UNKNOWN)
check("max has no case", max.caseBattery.level, Model.LEVEL_UNKNOWN)

// Between connect and the first battery packet the daemon sends the flag with no headset object at all.
const maxFresh = Model.parseStatus('{"connected":true,"is_headset":true,"noise_mode":1,"schema_version":1}')
check("max before any battery packet is still a headset", maxFresh.isHeadset, true)
check("max before any battery packet has no level", maxFresh.headset.level, Model.LEVEL_UNKNOWN)

// A daemon too old to send is_headset must keep drawing the pod rows rather than an empty headset row.
check("no is_headset key means earbuds", good.isHeadset, false)
check("no headset key is unknown, not zero", good.headset.level, Model.LEVEL_UNKNOWN)

// The capability keys, which are what stops the panel offering a control the hardware ignores.
const ap4 = Model.parseStatus('{"connected":true,"is_headset":false,"is_pro_series":false,"model_name":"AirPods 4","noise_mode":-1,"schema_version":1,"supports_adaptive":false,"supports_conversational_awareness":false,"supports_noise_control":false,"supports_noise_off":true,"supports_one_bud_anc":false}')
check("plain AirPods 4 has no listening modes", ap4.supportsNoiseControl, false)
check("plain AirPods 4 has no adaptive", ap4.supportsAdaptive, false)

const ap4anc = Model.parseStatus('{"connected":true,"is_headset":false,"is_pro_series":false,"model_name":"AirPods 4","noise_mode":1,"schema_version":1,"supports_adaptive":true,"supports_conversational_awareness":true,"supports_noise_control":true,"supports_noise_off":true,"supports_one_bud_anc":true}')
check("AirPods 4 with ANC hardware gets adaptive despite not being Pro", ap4anc.supportsAdaptive, true)
check("AirPods 4 with ANC hardware gets conversation awareness", ap4anc.supportsConversationalAwareness, true)
check("AirPods 4 with ANC hardware gets one-bud ANC", ap4anc.supportsOneBudANC, true)

const max2 = Model.parseStatus('{"connected":true,"is_headset":true,"is_pro_series":false,"model_name":"AirPods Max 2","noise_mode":1,"schema_version":1,"supports_adaptive":true,"supports_conversational_awareness":true,"supports_noise_control":true,"supports_noise_off":true,"supports_one_bud_anc":false}')
check("Max 2 has adaptive", max2.supportsAdaptive, true)
check("Max 2 has no second bud to keep ANC on", max2.supportsOneBudANC, false)

// A daemon older than the capability keys falls back to the Pro flag, which is what it used to gate on.
check("older daemon, Pro device, keeps adaptive", good.supportsAdaptive, true)
check("older daemon, Pro device, keeps one-bud ANC", good.supportsOneBudANC, true)
check("older daemon assumes listening modes exist", good.supportsNoiseControl, true)
const oldPlain = Model.parseStatus('{"connected":true,"is_pro_series":false,"noise_mode":1,"schema_version":1}')
check("older daemon, non-Pro device, hides adaptive", oldPlain.supportsAdaptive, false)
check("a false capability key is not treated as absent", ap4.supportsNoiseControl, false)

// An AirPods Pro 1 is the case the capability keys exist for: Pro, and no Adaptive.
const pro1 = Model.parseStatus('{"connected":true,"is_headset":false,"is_pro_series":true,"model_name":"AirPods Pro","noise_mode":1,"schema_version":1,"supports_adaptive":false,"supports_conversational_awareness":false,"supports_noise_control":true,"supports_noise_off":true,"supports_one_bud_anc":true}')
check("a Pro with an explicit false keeps the key, not the Pro flag", pro1.supportsAdaptive, false)
check("the same Pro still gets conversation awareness taken away", pro1.supportsConversationalAwareness, false)

// Three booleans rather than a status object, the shape Service.qml calls it with.
function modesFor(status) {
  return Model.availableModes(status.supportsNoiseControl, status.supportsNoiseOff, status.supportsAdaptive)
}
check("no listening modes at all on a plain AirPods 4", modesFor(ap4), [])
check("a Pro 1 gets Off, Transparency and ANC but no Adaptive", modesFor(pro1),
  [Model.NOISE_OFF, Model.NOISE_TRANSPARENCY, Model.NOISE_ANC])
check("a Pro 3 loses Off and keeps Adaptive", modesFor(good),
  [Model.NOISE_TRANSPARENCY, Model.NOISE_ADAPTIVE, Model.NOISE_ANC])
check("a Max 2 gets all four", modesFor(max2),
  [Model.NOISE_OFF, Model.NOISE_TRANSPARENCY, Model.NOISE_ADAPTIVE, Model.NOISE_ANC])

const powerbeats = Model.parseStatus('{"connected":true,"is_headset":false,"is_pro_series":false,"model_int":13,"model_name":"Powerbeats Pro","device_name":"Powerbeats Pro","noise_mode":-1,"schema_version":1}')
check("Powerbeats Pro is flagged isBeats", powerbeats.isBeats, true)
check("Powerbeats Pro is flagged isPowerbeats", powerbeats.isPowerbeats, true)

if (failures > 0) {
  console.log(failures + " failed")
  Deno.exit(1)
}
console.log("model.test.js: all checks passed")
