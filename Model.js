// No QML imports on purpose, so every function here runs in a plain JS harness.

var NOISE_OFF = 0
var NOISE_ANC = 1
var NOISE_TRANSPARENCY = 2
var NOISE_ADAPTIVE = 3

var LID_OPEN = 0
var LID_CLOSED = 1
var LID_UNKNOWN = 2

var EAR_PAUSE_ONE_OUT = 0
var EAR_PAUSE_BOTH_OUT = 1
var EAR_DISABLED = 2

// Highest schema_version this panel knows how to read.
var SUPPORTED_SCHEMA = 1

// Level the daemon reports when a pod or the case has not been heard from.
var LEVEL_UNKNOWN = -1

// noise_mode before the daemon has identified the device.
var NOISE_UNKNOWN = -1

// How many ear-detection behaviours exist, so callers can cycle without a bare 3.
var EAR_BEHAVIOR_COUNT = 3

// nf-md-check U+F012C, the one candidate glyph that rendered on the box.
var GLYPH_CHECK = "\uDB80\uDD2C"

// The real file is about 1 KB; the read is capped one byte past this so an oversized file is detectable.
var MAX_STATUS_BYTES = 65536

// UTF-8 widths: below 0x80 one byte, below 0x800 two, the rest of the basic plane three.
var UTF8_ONE_BYTE_BELOW = 0x80
var UTF8_TWO_BYTES_BELOW = 0x800
var SURROGATE_FIRST = 0xD800
var SURROGATE_LAST = 0xDFFF

// A key the Rust daemon always writes and the C++ daemon never did, so its document says `librepods set` exists.
var CURRENT_DAEMON_KEY = "advert_intake"

// The advert_intake word of a daemon that holds the pods' keys and cannot hear their adverts.
var ADVERT_UNAVAILABLE = "unavailable"

// Found on PATH, where setup installs it.
var SET_PROGRAM = "librepods"

// Longest error the panel will show inside a row, and the cut that leaves room for the ellipsis.
var MAX_ERROR_CHARS = 140
var ELIDED_ERROR_CHARS = 137

function defaultPod() {
  return { level: LEVEL_UNKNOWN, charging: false, inEar: false }
}

// Full shape on every path, so the panel never reads undefined off a parse failure.
function defaultStatus() {
  return {
    ok: false,
    lastError: "",
    schemaVersion: 0,
    schemaTooNew: false,
    daemonCurrent: false,
    advertIntake: "",
    connected: false,
    deviceName: "",
    modelName: "",
    isProSeries: false,
    isHeadset: false,
    supportsNoiseOff: true,
    supportsNoiseControl: true,
    supportsAdaptive: false,
    supportsConversationalAwareness: false,
    supportsOneBudANC: false,
    noiseMode: NOISE_UNKNOWN,
    adaptiveNoiseLevel: 0,
    oneBudANC: false,
    conversationalAwareness: false,
    earDetectionBehavior: EAR_PAUSE_ONE_OUT,
    lidState: LID_UNKNOWN,
    left: defaultPod(),
    right: defaultPod(),
    caseBattery: { level: LEVEL_UNKNOWN, charging: false },
    headset: { level: LEVEL_UNKNOWN, charging: false }
  }
}

function intOr(value, fallback) {
  var n = parseInt(value, 10)
  return isFinite(n) ? n : fallback
}

// A daemon older than the capability keys sends nothing, which is not the same as false.
function boolOr(value, fallback) {
  return value === undefined ? fallback : value === true
}

function podFrom(raw) {
  var pod = defaultPod()
  if (!raw || typeof raw !== "object") return pod
  // available:false means the daemon has stopped hearing from this pod, so its charging and in_ear are stale too.
  if (raw.available !== true) return pod
  pod.level = intOr(raw.level, LEVEL_UNKNOWN)
  pod.charging = raw.charging === true
  pod.inEar = raw.in_ear === true
  return pod
}

// The bytes a decoded line took on disk, because the reader cuts the file at a byte count.
function utf8Length(text) {
  var bytes = 0
  for (var i = 0; i < text.length; i++) {
    var unit = text.charCodeAt(i)
    if (unit < UTF8_ONE_BYTE_BELOW) bytes += 1
    // Each half of a surrogate pair counts two, so a four byte character counts four.
    else if (unit < UTF8_TWO_BYTES_BELOW || (unit >= SURROGATE_FIRST && unit <= SURROGATE_LAST)) bytes += 2
    else bytes += 3
  }
  return bytes
}

// The whole of $XDG_STATE_HOME/librepods/status.json, one line, keys sorted by QJsonObject rather than by insert order:
// {"adaptive_level_changes_total":0,"adaptive_noise_level":50,"ca_changes_total":0,
//  "case":{"available":true,"charging":false,"level":100},"connect_calls_total":0,
//  "connect_failures_total":0,"connected":true,"conversational_awareness":true,
//  "device_name":"GM’s AirPods Pro","disconnect_calls_total":0,
//  "disconnect_failures_total":0,"ear_detection_behavior":0,
//  "ear_detection_changes_total":0,"forget_calls_total":0,"is_pro_series":true,
//  "left":{"available":true,"charging":false,"in_ear":false,"level":79},
//  "lid_state":2,"model_int":11,"model_name":"AirPods Pro 3","model_number":"A3064",
//  "noise_control_changes_total":0,"noise_mode":1,"one_bud_anc_changes_total":0,
//  "one_bud_anc_mode":true,"reconnect_attempts_total":0,"reconnect_failures_total":0,
//  "reopen_calls_total":0,
//  "right":{"available":true,"charging":true,"in_ear":false,"level":100},
//  "schema_version":1,"supports_adaptive":true,"supports_conversational_awareness":true,
//  "supports_noise_control":true,"supports_noise_off":false,"supports_one_bud_anc":true}
// A headset (AirPods Max) instead sends "is_headset":true and "headset":{"available":true,"charging":false,"level":100}, with no pods and no case.
function parseStatus(raw) {
  var status = defaultStatus()
  if (utf8Length(String(raw || "")) > MAX_STATUS_BYTES) {
    status.lastError = "The librepods status file is too large to read"
    return status
  }
  var text = String(raw || "").trim()
  if (text === "") {
    status.lastError = "The librepods status file is empty"
    return status
  }

  var parsed
  try {
    parsed = JSON.parse(text)
  } catch (e) {
    status.lastError = "Could not read the librepods status file"
    return status
  }
  if (!parsed || typeof parsed !== "object" || parsed.schema_version === undefined) {
    status.lastError = "The librepods status file carried no schema_version"
    return status
  }

  status.schemaVersion = intOr(parsed.schema_version, 0)
  if (status.schemaVersion > SUPPORTED_SCHEMA) {
    // Newer daemon: report the version rather than draw fields we may be misreading.
    status.schemaTooNew = true
    status.lastError = "librepods speaks status schema " + status.schemaVersion + ", this panel reads " + SUPPORTED_SCHEMA
    return status
  }

  status.ok = true
  status.daemonCurrent = parsed[CURRENT_DAEMON_KEY] !== undefined
  status.advertIntake = String(parsed.advert_intake || "")
  status.connected = parsed.connected === true
  status.deviceName = String(parsed.device_name || "")
  status.modelName = String(parsed.model_name || "")
  status.isProSeries = parsed.is_pro_series === true
  // Older daemons send neither key, so this stays false and the pod rows keep drawing.
  status.isHeadset = parsed.is_headset === true
  // Older daemons do not send this, and every model before the Pro 3 had Off.
  status.supportsNoiseOff = parsed.supports_noise_off !== false
  // A daemon without these keys falls back to what the panel used to gate on.
  status.supportsNoiseControl = boolOr(parsed.supports_noise_control, true)
  status.supportsAdaptive = boolOr(parsed.supports_adaptive, status.isProSeries)
  status.supportsConversationalAwareness = boolOr(parsed.supports_conversational_awareness, status.isProSeries)
  status.supportsOneBudANC = boolOr(parsed.supports_one_bud_anc, status.isProSeries)
  status.noiseMode = intOr(parsed.noise_mode, NOISE_UNKNOWN)
  status.adaptiveNoiseLevel = intOr(parsed.adaptive_noise_level, 0)
  status.oneBudANC = parsed.one_bud_anc_mode === true
  status.conversationalAwareness = parsed.conversational_awareness === true
  status.earDetectionBehavior = intOr(parsed.ear_detection_behavior, EAR_PAUSE_ONE_OUT)
  status.lidState = intOr(parsed.lid_state, LID_UNKNOWN)
  status.left = podFrom(parsed.left)
  status.right = podFrom(parsed.right)
  var caseRaw = podFrom(parsed["case"])
  status.caseBattery = { level: caseRaw.level, charging: caseRaw.charging }
  var headsetRaw = podFrom(parsed.headset)
  status.headset = { level: headsetRaw.level, charging: headsetRaw.charging }
  return status
}

// The mark the panel draws: a headset is the Max whatever else it reports, then the Pro, then the buds.
function markVariant(isHeadset, isProSeries) {
  if (isHeadset) return "max"
  if (isProSeries) return "pro"
  return "buds"
}

// The one list both the cycle and the panel rows are built from, in the order the panel draws them.
function availableModes(hasNoiseControl, hasOff, hasAdaptive) {
  // AirPods 1, 2, 3 and the plain AirPods 4 have no listening modes at all.
  if (!hasNoiseControl) return []
  var modes = []
  if (hasOff) modes.push(NOISE_OFF)
  modes.push(NOISE_TRANSPARENCY)
  if (hasAdaptive) modes.push(NOISE_ADAPTIVE)
  modes.push(NOISE_ANC)
  return modes
}

function noiseModeName(mode) {
  if (mode === NOISE_OFF) return "Off"
  if (mode === NOISE_ANC) return "Noise Cancellation"
  if (mode === NOISE_TRANSPARENCY) return "Transparency"
  if (mode === NOISE_ADAPTIVE) return "Adaptive"
  return "Unknown"
}

// What `librepods set` takes, and the librepods-ctl verb an older daemon takes for the same thing.
function changeOf(key, value, verb) {
  return { key: key, value: value, verb: verb }
}

// Indexed by mode; librepods-ctl takes the same word after noise:.
function noiseChange(mode) {
  var values = ["off", "anc", "transparency", "adaptive"]
  if (mode < 0 || mode >= values.length) return null
  return changeOf("noise", values[mode], "noise:" + values[mode])
}

// Indexed by behaviour; librepods-ctl takes the same word after ear:.
function earChange(behavior) {
  var values = ["one", "both", "off"]
  if (behavior < 0 || behavior >= values.length) return null
  return changeOf("ear_detection", values[behavior], "ear:" + values[behavior])
}

// The two switches, each with the name librepods-ctl's verbs give it; values is indexed off then on.
function switchChange(key, on) {
  var keys = ["conversational_awareness", "one_bud_anc"]
  var verbNames = ["ca", "onebud"]
  var values = ["false", "true"]
  var at = keys.indexOf(key)
  if (at < 0) return null
  return changeOf(key, values[on ? 1 : 0], verbNames[at] + (on ? ":on" : ":off"))
}

function adaptiveChange(level) {
  return changeOf("adaptive_level", String(level), "adaptive:" + level)
}

// The released plugin runs [ctlPath or "librepods-ctl", verb], and an older daemon gets exactly that.
function commandFor(daemonCurrent, ctlPath, change) {
  if (daemonCurrent) return [SET_PROGRAM, "set", change.key, change.value]
  return [String(ctlPath || "librepods-ctl"), change.verb]
}

// Only the current daemon writes advert_intake, so an older daemon's document never shows the hint.
function advertHint(status) {
  return !status.connected && status.advertIntake === ADVERT_UNAVAILABLE
}

function earDetectionName(behavior) {
  if (behavior === EAR_PAUSE_ONE_OUT) return "Pause when one is out"
  if (behavior === EAR_PAUSE_BOTH_OUT) return "Pause when both are out"
  if (behavior === EAR_DISABLED) return "Never pause"
  return "Unknown"
}

function levelText(level) {
  return level === LEVEL_UNKNOWN ? "--" : String(level) + "%"
}

// 0 to 1 for the meter; an unknown level draws an empty track rather than a full one.
function levelFraction(level) {
  if (level === LEVEL_UNKNOWN) return 0
  return Math.max(0, Math.min(100, level)) / 100
}

function lidText(lidState) {
  if (lidState === LID_OPEN) return "Open"
  if (lidState === LID_CLOSED) return "Closed"
  return ""
}

function podMeta(pod) {
  if (pod.charging) return "Charging"
  if (pod.inEar) return "In ear"
  return ""
}

var DAEMON_UNAVAILABLE = 69
var DAEMON_DOWN_TEXT = "librepods is not running"

// Collapse stderr like "Refused\nTry again\n" into one line for the panel.
function elideError(text) {
  var value = String(text || "").replace(/\s+/g, " ").trim()
  return value.length > MAX_ERROR_CHARS ? value.substring(0, ELIDED_ERROR_CHARS) + "…" : value
}

// The refusal is the first line: "That setting does not take this value.\nValues: off, anc, transparency, adaptive, cycle\n".
function refusalText(stderr) {
  return elideError(String(stderr || "").split("\n")[0])
}

function commandFailureText(exitCode, isSetProgram, stderr) {
  if (exitCode === DAEMON_UNAVAILABLE) return DAEMON_DOWN_TEXT
  return isSetProgram
    ? refusalText(stderr || "librepods refused the change")
    : elideError(stderr || "librepods-ctl rejected the command")
}
