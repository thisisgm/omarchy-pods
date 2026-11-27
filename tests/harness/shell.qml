import QtQuick
import Quickshell
import "Model.js" as Model

// Runs the plugin's own Service.qml against the state directory the run made and prints every change it reports.
ShellRoot {
  id: harness

  readonly property int defaultSeconds: 10
  readonly property int changeDelayMs: 1000
  readonly property int msPerSecond: 1000
  // Distinct codes so a run that refused to start is never read as a run that passed.
  readonly property int exitRefused: 2

  readonly property string stateHome: String(Quickshell.env("XDG_STATE_HOME") || "")
  readonly property string secondsText: String(Quickshell.env("PODS_HARNESS_SECONDS") || "")
  readonly property string change: String(Quickshell.env("PODS_HARNESS_CHANGE") || "")
  readonly property string runtimeDir: String(Quickshell.env("XDG_RUNTIME_DIR") || "")

  property var service: null

  // Deferred, because the engine is torn down on exit and this runs while the root is still being made.
  function refuse(reason) {
    console.log("harness refused: " + reason)
    Qt.callLater(function () { Qt.exit(harness.exitRefused) })
  }

  function seconds() {
    if (secondsText === "") return defaultSeconds
    return /^[1-9][0-9]*$/.test(secondsText) ? parseInt(secondsText, 10) : -1
  }

  function parentOf(path) {
    return path.replace(/\/+$/, "").replace(/\/[^\/]*$/, "")
  }

  // The client finds its daemon through XDG_RUNTIME_DIR, so a change is only sent from a runtime directory the run made.
  function runtimeIsPrivate() {
    return parentOf(stateHome) !== "" && parentOf(runtimeDir) === parentOf(stateHome)
  }

  // The released Model.js has only the verb, and its word after noise: is the word set takes.
  function noiseWord(mode) {
    return Model.noiseChange ? Model.noiseChange(mode).value : Model.noiseModeVerb(mode).split(":")[1]
  }

  // The noise mode the change names as a key and a value, such as "noise anc", or -1; only noise changes are sent.
  function changeMode() {
    for (var mode = Model.NOISE_OFF; mode <= Model.NOISE_ADAPTIVE; mode++) {
      if ("noise " + noiseWord(mode) === change) return mode
    }
    return -1
  }

  function report() {
    console.log("harness " + JSON.stringify({
      daemonReachable: service.daemonReachable,
      connected: service.connected,
      lastError: service.lastError,
      actionStatus: service.actionStatus,
      schemaUnsupported: service.schemaUnsupported,
      noiseMode: service.noiseMode
    }))
  }

  Component.onCompleted: {
    // Checked before Service exists, because Service reads its file as soon as it is made.
    if (stateHome.charAt(0) !== "/") return refuse("XDG_STATE_HOME must be the absolute path of a directory this run made")
    if (seconds() < 0) return refuse("PODS_HARNESS_SECONDS must be a whole number of seconds above zero, got " + secondsText)
    if (change !== "" && changeMode() < 0) return refuse("PODS_HARNESS_CHANGE must be the key noise and a mode, such as noise anc, got " + change)
    if (change !== "" && !runtimeIsPrivate()) return refuse("PODS_HARNESS_CHANGE needs XDG_RUNTIME_DIR beside XDG_STATE_HOME in the run's own directory, or the change could reach real AirPods")
    service = serviceComponent.createObject(harness)
    report()
    endTimer.start()
    if (change !== "") changeTimer.start()
  }

  Component {
    id: serviceComponent
    Service {}
  }

  Connections {
    target: harness.service
    function onDaemonReachableChanged() { harness.report() }
    function onConnectedChanged() { harness.report() }
    function onLastErrorChanged() { harness.report() }
    function onActionStatusChanged() { harness.report() }
    function onSchemaUnsupportedChanged() { harness.report() }
    function onNoiseModeChanged() { harness.report() }
  }

  Timer {
    id: changeTimer
    interval: harness.changeDelayMs
    onTriggered: {
      console.log("harness sent " + harness.change)
      harness.service.setNoiseMode(harness.changeMode())
    }
  }

  Timer {
    id: endTimer
    interval: harness.seconds() * harness.msPerSecond
    onTriggered: Qt.exit(0)
  }
}
