// Run with: deno run --allow-read tests/model.test.js
// Model.js has no exports, so it is evaluated here rather than imported.

const source = Deno.readTextFileSync(new URL("../Model.js", import.meta.url))
const Model = new Function(
  source + "; return { parseStatus, podFrom, defaultPod, noiseChange, earChange, switchChange, adaptiveChange, commandFor, refusalText, advertHint, levelFraction, levelText, podMeta, elideError, availableModes, markVariant, NOISE_OFF, NOISE_ANC, NOISE_TRANSPARENCY, NOISE_ADAPTIVE, LEVEL_UNKNOWN, NOISE_UNKNOWN, EAR_PAUSE_ONE_OUT, EAR_PAUSE_BOTH_OUT, EAR_DISABLED, LID_UNKNOWN, MAX_ERROR_CHARS, MAX_STATUS_BYTES }"
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

// The panel reads at most one byte past the cap, so a longer line means an oversized file.
const huge = Model.parseStatus('{"schema_version":1,"pad":"' + "x".repeat(Model.MAX_STATUS_BYTES) + '"}')
check("oversized line is refused", huge.ok, false)
check("oversized line says why", huge.lastError, "The librepods status file is too large to read")

// A minimal line padded to an exact length in UTF-8 bytes.
function lineOfBytes(bytes, padChar, padCharBytes) {
  const frame = '{"schema_version":1,"pad":""}'
  const count = (bytes - frame.length) / padCharBytes
  if (!Number.isInteger(count)) throw new Error("lineOfBytes: " + bytes + " bytes is not the frame plus whole pad characters")
  return frame.slice(0, -2) + padChar.repeat(count) + '"}'
}
check("a line of exactly the bound is read", Model.parseStatus(lineOfBytes(Model.MAX_STATUS_BYTES, "x", 1)).ok, true)
check("one byte over the bound is refused", Model.parseStatus(lineOfBytes(Model.MAX_STATUS_BYTES + 1, "x", 1)).lastError,
  "The librepods status file is too large to read")
// head counts bytes, so a line of two byte characters is over the bound at half the characters.
check("a line of two byte characters over the bound is refused",
  Model.parseStatus(lineOfBytes(Model.MAX_STATUS_BYTES + 3, "é", 2)).lastError,
  "The librepods status file is too large to read")
// U+2019 is three bytes and U+1D11E four, the second written as a surrogate pair.
check("a line of three byte characters under the bound is read",
  Model.parseStatus(lineOfBytes(Model.MAX_STATUS_BYTES - 2, "\u2019", 3)).ok, true)
check("a line of three byte characters over the bound is refused",
  Model.parseStatus(lineOfBytes(Model.MAX_STATUS_BYTES + 1, "\u2019", 3)).lastError,
  "The librepods status file is too large to read")
check("a line of four byte characters under the bound is read",
  Model.parseStatus(lineOfBytes(Model.MAX_STATUS_BYTES - 3, "\u{1D11E}", 4)).ok, true)
check("a line of four byte characters over the bound is refused",
  Model.parseStatus(lineOfBytes(Model.MAX_STATUS_BYTES + 1, "\u{1D11E}", 4)).lastError,
  "The librepods status file is too large to read")
// The reader keeps one byte past the bound, and Qt decodes the cut character as U+FFFD, as TextDecoder does.
const cutRead = new TextDecoder().decode(
  new TextEncoder().encode(lineOfBytes(Model.MAX_STATUS_BYTES + 4, "\u2019", 3)).slice(0, Model.MAX_STATUS_BYTES + 1))
check("the cut read ends inside a character", cutRead.endsWith("\uFFFD"), true)
check("a read cut inside a three byte character is refused", Model.parseStatus(cutRead).lastError,
  "The librepods status file is too large to read")
check("a line cut in the middle is an error and not a crash", Model.parseStatus(live.slice(0, live.length / 2)).lastError,
  "Could not read the librepods status file")

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

// Each change is what `librepods set` takes and the librepods-ctl verb for the same thing.
for (const [mode, value] of [[Model.NOISE_OFF, "off"], [Model.NOISE_ANC, "anc"],
  [Model.NOISE_TRANSPARENCY, "transparency"], [Model.NOISE_ADAPTIVE, "adaptive"]]) {
  check("noiseChange " + value, Model.noiseChange(mode), { key: "noise", value: value, verb: "noise:" + value })
}
check("noiseChange of an unknown mode is null", Model.noiseChange(Model.NOISE_UNKNOWN), null)
check("noiseChange past the end is null", Model.noiseChange(4), null)
for (const [behavior, value] of [[Model.EAR_PAUSE_ONE_OUT, "one"], [Model.EAR_PAUSE_BOTH_OUT, "both"], [Model.EAR_DISABLED, "off"]]) {
  check("earChange " + value, Model.earChange(behavior), { key: "ear_detection", value: value, verb: "ear:" + value })
}
check("earChange below the start is null", Model.earChange(-1), null)
check("earChange past the end is null", Model.earChange(3), null)
check("switchChange conversational_awareness on", Model.switchChange("conversational_awareness", true),
  { key: "conversational_awareness", value: "true", verb: "ca:on" })
check("switchChange conversational_awareness off", Model.switchChange("conversational_awareness", false),
  { key: "conversational_awareness", value: "false", verb: "ca:off" })
check("switchChange one_bud_anc on", Model.switchChange("one_bud_anc", true), { key: "one_bud_anc", value: "true", verb: "onebud:on" })
check("switchChange one_bud_anc off", Model.switchChange("one_bud_anc", false), { key: "one_bud_anc", value: "false", verb: "onebud:off" })
check("switchChange of a key that is not a switch is null", Model.switchChange("noise", true), null)
check("adaptiveChange 40", Model.adaptiveChange(40), { key: "adaptive_level", value: "40", verb: "adaptive:40" })
check("adaptiveChange 0", Model.adaptiveChange(0), { key: "adaptive_level", value: "0", verb: "adaptive:0" })

// What the released plugin sends to librepods-ctl, one row per control and value.
const releasedVerbs = ["noise:off", "noise:anc", "noise:transparency", "noise:adaptive", "ear:one", "ear:both", "ear:off",
  "ca:on", "ca:off", "onebud:on", "onebud:off", "adaptive:0", "adaptive:40", "adaptive:100"]
const everyChange = [0, 1, 2, 3].map(Model.noiseChange).concat([0, 1, 2].map(Model.earChange), [
  Model.switchChange("conversational_awareness", true), Model.switchChange("conversational_awareness", false),
  Model.switchChange("one_bud_anc", true), Model.switchChange("one_bud_anc", false),
  Model.adaptiveChange(0), Model.adaptiveChange(40), Model.adaptiveChange(100)])
check("every_old_verb_is_the_verb_the_released_plugin_sends", everyChange.map(function (change) { return change && change.verb }), releasedVerbs)

// The released plugin runs [ctlPath or "librepods-ctl", verb]; a current daemon takes `librepods set`.
const anc = Model.noiseChange(Model.NOISE_ANC)
check("commandFor an old daemon", Model.commandFor(false, "", anc), ["librepods-ctl", "noise:anc"])
check("commandFor an old daemon with a ctlPath", Model.commandFor(false, "/opt/pods/librepods-ctl", anc), ["/opt/pods/librepods-ctl", "noise:anc"])
check("commandFor a current daemon", Model.commandFor(true, "", anc), ["librepods", "set", "noise", "anc"])
check("commandFor a current daemon ignores the ctlPath", Model.commandFor(true, "/opt/pods/librepods-ctl", anc), ["librepods", "set", "noise", "anc"])

// librepods set prints the refusal as one sentence, then for a bad value a line of the values it takes.
check("refusalText drops the Values line", Model.refusalText("That setting does not take this value.\nValues: off, anc, transparency, adaptive, cycle\n"),
  "That setting does not take this value.")
check("refusalText of one sentence", Model.refusalText("The AirPods are not connected.\n"), "The AirPods are not connected.")
check("refusalText of a long first line fits the row", Model.refusalText("x ".repeat(200) + "\nValues: a").length <= Model.MAX_ERROR_CHARS, true)
check("refusalText copes with nothing", Model.refusalText(null), "")

// Only the current daemon writes this key, so a document without it is the old daemon's.
check("daemonCurrent of the line from the box", good.daemonCurrent, false)
check("daemonCurrent of a line with advert_intake", Model.parseStatus('{"advert_intake":"paused","connected":false,"schema_version":1}').daemonCurrent, true)
check("daemonCurrent of a file that does not parse", Model.parseStatus("").daemonCurrent, false)

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

// The mark of each family: a headset is the Max whatever else it reports, then the Pro, then the buds.
check("markVariant of neither flag", Model.markVariant(false, false), "buds")
check("markVariant of a Pro", Model.markVariant(false, true), "pro")
check("markVariant of a headset", Model.markVariant(true, false), "max")
check("markVariant of both flags", Model.markVariant(true, true), "max")

// The line the C++ 1.3.6 daemon wrote with nothing connected, then the same line with the flag it sets for a Pro or a Max.
const oldLine = Deno.readTextFileSync(new URL("../crates/core/tests/fixtures/status-disconnected.json", import.meta.url))
function oldDocument(flags) {
  return Model.parseStatus(JSON.stringify(Object.assign(JSON.parse(oldLine), flags)))
}
for (const [name, status, mark] of [
  ["nothing connected", oldDocument({}), "buds"],
  ["a Pro", oldDocument({ is_pro_series: true }), "pro"],
  ["a Max", oldDocument({ is_headset: true }), "max"],
  ["the line from the box, older than is_headset", good, "pro"],
]) {
  check("a_document_of_the_old_daemon_gives_the_mark_it_gave: " + name, Model.markVariant(status.isHeadset, status.isProSeries), mark)
}
check("a document of the old daemon is not current", oldDocument({}).daemonCurrent, false)
check("an_old_document_keeps_the_ordinary_guidance", Model.advertHint(oldDocument({})), false)

// The pods away and the daemon holding their keys, then with the link up and from a daemon too old to say.
function intakeDocument(word, connected) {
  return Model.parseStatus(JSON.stringify({ advert_intake: word, connected: connected, schema_version: 1 }))
}
check("advertHint while listening", Model.advertHint(intakeDocument("listening", false)), false)
check("advertHint while paused", Model.advertHint(intakeDocument("paused", false)), false)
check("advertHint while unavailable", Model.advertHint(intakeDocument("unavailable", false)), true)
check("advertHint with the pods connected", Model.advertHint(intakeDocument("unavailable", true)), false)
check("advertHint of a word this panel does not know", Model.advertHint(intakeDocument("deaf", false)), false)
check("advertHint of a file that does not parse", Model.advertHint(Model.parseStatus("")), false)
check("a_document_without_the_new_keys_hides_their_rows: advertIntake", good.advertIntake, "")
check("a_document_without_the_new_keys_hides_their_rows: no hint", Model.advertHint(good), false)

if (failures > 0) {
  console.log(failures + " failed")
  Deno.exit(1)
}
console.log("model.test.js: all checks passed")
