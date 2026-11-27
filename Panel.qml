pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "Model.js" as Model

Panel {
  id: root
  moduleName: "io.github.thisisgm.omapods"
  ipcTarget: "omapods"
  manageIpc: false

  property int cursorIndex: 0
  property bool cursorActive: false
  property bool cursorFromKeyboard: false

  readonly property var hideSetting: setting("hideWhenDisconnected", true)
  readonly property bool hideWhenDisconnected: typeof hideSetting === "boolean" ? hideSetting : true
  readonly property color popupText: Commons.Color.popups.text
  readonly property color popupBackground: Commons.Color.popups.background
  readonly property color sourceAccent: inkColor(Model.huedInk(Commons.Color.accent, popupText))
  readonly property color sourceUrgent: inkColor(Model.huedInk(Commons.Color.urgent, popupText))
  readonly property color focusFill: Style.hoverFillFor(popupText, sourceAccent, sourceUrgent)
  readonly property color trackFill: Style.selectedFillFor(popupText, sourceAccent, sourceUrgent)
  readonly property color toggleOffFill: Style.normalFillFor(popupText, sourceAccent, sourceUrgent)
  readonly property var inks: Model.panelInks({
    background: popupBackground,
    foreground: popupText,
    accent: sourceAccent,
    urgent: sourceUrgent,
    focusFill: focusFill,
    focus: Style.hoverBorderFor(popupText, sourceAccent, sourceUrgent),
    track: trackFill,
    toggleOff: Style.normalBorderFor(popupText, sourceAccent, sourceUrgent),
    toggleOn: Style.selectedBorderFor(popupText, sourceAccent, sourceUrgent),
    offFill: toggleOffFill,
    onFill: trackFill
  }, dimOpacity)
  readonly property color foreground: inkColor(inks.text)
  readonly property color quiet: inkColor(inks.quiet)
  readonly property color urgent: inkColor(inks.error)
  readonly property color separator: inkColor(inks.separator)
  readonly property var rowColors: ({
    foreground: foreground,
    quiet: quiet,
    focusFill: focusFill,
    focusInk: inkColor(inks.focus),
    trackFill: trackFill,
    toggleOffFill: toggleOffFill,
    meterTrack: inkColor(inks.meterTrack),
    meterFill: inkColor(inks.meterFill),
    lowBattery: inkColor(inks.lowBattery),
    glyph: inkColor(inks.glyph),
    toggleOn: inkColor(inks.toggleOn),
    toggleOff: inkColor(inks.toggleOff),
    toggleOnKnob: inkColor(inks.toggleOnKnob),
    toggleOffKnob: inkColor(inks.toggleOffKnob),
    sliderTrack: inkColor(inks.sliderTrack),
    sliderFill: inkColor(inks.sliderFill)
  })

  readonly property bool clearBar: bar ? bar.transparent : false
  readonly property var barInks: Model.barInks(
    clearBar ? barForeground : Commons.Color.bar.text,
    Commons.Color.bar.background, Commons.Color.bar.text, Commons.Color.background,
    clearBar ? "none" : bar?.shell?.barConfig?.background,
    !Style.reduceMotion, dimOpacity)
  readonly property string fontFamily: Style.font.family
  readonly property real dimOpacity: 0.6
  readonly property PanelContent panelContent: panel.loadedContent as PanelContent
  // Say nothing extra once a section already explains the state.
  readonly property bool guidanceVisible: !pods.hasAirPods && !pods.hasBattery && !pods.schemaUnsupported

  property int phraseIndex: 0

  readonly property int adaptiveStepPercent: 5
  readonly property int phraseIntervalMs: 2800

  // Ten, matching the stock panels.
  readonly property var activePhrases: [
    "All gone Pete Tong",
    "Essential Selection",
    "Genius Bar stumped",
    "Warming the decks",
    "Hands in the air",
    "Ibiza incoming",
    "Rinsing the low end",
    "Big tune loading",
    "Cued and counting in",
    "Ecosystem escaped"
  ]
  readonly property string heroPhraseText: activePhrases[phraseIndex % activePhrases.length]

  readonly property bool adaptiveVisible: pods.hasAirPods && pods.supportsAdaptive && pods.noiseMode === Model.NOISE_ADAPTIVE
  // Three separate capabilities, because a Max 2 has Conversation Awareness and no second bud.
  readonly property bool modesVisible: pods.hasAirPods && modes.length > 0
  readonly property bool caVisible: pods.hasAirPods && pods.supportsConversationalAwareness
  readonly property bool oneBudVisible: pods.hasAirPods && pods.supportsOneBudANC
  readonly property string lidLabel: Model.lidText(pods.lidState)

  // The daemon names the family, so the mark is the shape of the device actually connected.
  readonly property string podsVariant: Model.markVariant(pods.isHeadset, pods.isProSeries)

  // The daemon decides which modes this device has, so no row is drawn that the hardware ignores.
  readonly property var modes: pods.availableModes()

  // Rebuilt whenever a section appears, so j and k never land on a hidden control.
  readonly property var cursorRows: {
    var rows = []
    if (!pods.hasAirPods) return rows
    for (let i = 0; i < modes.length; i++) rows.push("mode:" + modes[i])
    if (adaptiveVisible) rows.push("adaptive")
    if (caVisible) rows.push("ca")
    if (oneBudVisible) rows.push("onebud")
    rows.push("ear")
    return rows
  }

  readonly property string cursorRow: cursorRows.length === 0
    ? ""
    : cursorRows[Math.max(0, Math.min(cursorIndex, cursorRows.length - 1))]

  function inkColor(ink): color {
    return Qt.rgba(ink.r, ink.g, ink.b, ink.a)
  }

  function rowHasCursor(name) {
    return cursorActive && cursorRow === name
  }

  function moveCursor(dy) {
    cursorActive = true
    if (cursorRows.length === 0) return
    cursorIndex = Math.max(0, Math.min(cursorRows.length - 1, cursorIndex + dy))
  }

  function nudgeCursor(dx) {
    if (cursorRow === "adaptive") pods.setAdaptiveNoiseLevel(pods.adaptiveNoiseLevel + dx * adaptiveStepPercent)
  }

  function activateCursor() {
    var name = cursorRow
    if (name.indexOf("mode:") === 0) pods.setNoiseMode(parseInt(name.substring(5), 10))
    else if (name === "ca") pods.setConversationalAwareness(!pods.conversationalAwareness)
    else if (name === "onebud") pods.setOneBudANC(!pods.oneBudANC)
    else if (name === "ear") pods.cycleEarDetection()
  }

  function focusRow(name) {
    var at = cursorRows.indexOf(name)
    if (at < 0) return
    cursorFromKeyboard = false
    cursorActive = true
    cursorIndex = at
  }

  function scrollRowIntoView(row: CursorSurface) {
    if (!cursorFromKeyboard) return
    Qt.callLater(function () {
      if (!root.opened || !root.cursorFromKeyboard || !root.panelContent || !row || !row.visible || !row.hasCursor) return
      const flick = root.panelContent.flick
      const margin = Style.spacing.md
      const top = row.mapToItem(flick.contentItem, 0, 0).y
      const bottom = top + row.height
      const maxY = Math.max(0, flick.contentHeight - flick.height)
      if (top < flick.contentY + margin) flick.contentY = Math.max(0, top - margin)
      else if (bottom > flick.contentY + flick.height - margin)
        flick.contentY = Math.min(maxY, bottom + margin - flick.height)
    })
  }

  visible: !hideWhenDisconnected || pods.hasAirPods || pods.hasBattery
  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  onOpenedChanged: if (opened) {
    cursorActive = false
    cursorIndex = 0
    pods.refresh()
    Qt.callLater(function () {
      if (!root.opened || !root.panelContent) return
      root.panelContent.flick.contentY = 0
      root.panelContent.forceActiveFocus()
    })
  }

  Service {
    id: pods
    settings: root.settings
  }

  Commons.ShellIpc {
    target: root.ipcTarget
    function open(): void { root.open() }
    function close(): void { root.close() }
    function show(): void { root.open() }
    function hide(): void { root.close() }
    function toggle(): void { root.toggle() }
    function refresh(): string { pods.refresh(); return "ok" }
    function noise(): string { pods.cycleNoiseMode(); return "ok" }
    function status(): string { return Model.noiseModeName(pods.noiseMode) }
  }

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    iconComponent: Component {
      Item {
        AirPodsIcon {
          x: (parent.width - width) / 2
          y: (parent.height - height) / 2
          iconSize: button.iconSize
          color: root.inkColor(pods.hasAirPods ? root.barInks.mark : root.barInks.quiet)
          variant: root.podsVariant
        }
      }
    }
    onPressed: function (buttonCode) {
      if (buttonCode === Qt.RightButton) pods.cycleNoiseMode()
      else root.toggle()
    }
  }

  KeyboardPanel {
    id: panel
    anchorItem: button
    owner: root
    bar: root.bar
    open: root.opened
    focusTarget: panel.loadedContent
    contentWidth: panel.fittedContentWidth(Style.space(380))
    contentHeight: panel.fittedContentHeight(panel.loadedContentHeight, Style.space(560))

    content: PanelContent {}
  }

  component PanelContent: PanelKeyCatcher {
    property alias flick: panelFlick
    implicitHeight: column.implicitHeight
    anchors.fill: parent
    onMoveRequested: function (dx, dy) {
      root.cursorFromKeyboard = true
      if (!root.cursorActive) { root.cursorActive = true; return }
      if (dy !== 0) root.moveCursor(dy)
      else if (dx !== 0) root.nudgeCursor(dx)
    }
    onActivateRequested: if (root.cursorActive) root.activateCursor()
    onCloseRequested: root.close()
    onTabRequested: function (direction) { root.switchPanel(direction) }
    onTextKey: function (t) {
      var key = String(t).toLowerCase()
      if (key === "r") pods.refresh()
      else if (!pods.hasAirPods) return
      // The mode keys need no capability check of their own: setNoiseMode drops a mode this device does not have.
      else if (key === "o") pods.setNoiseMode(Model.NOISE_OFF)
      else if (key === "t") pods.setNoiseMode(Model.NOISE_TRANSPARENCY)
      else if (key === "n") pods.setNoiseMode(Model.NOISE_ANC)
      else if (key === "a") pods.setNoiseMode(Model.NOISE_ADAPTIVE)
      else if (key === "c" && pods.supportsConversationalAwareness) pods.setConversationalAwareness(!pods.conversationalAwareness)
      else if (key === "b" && pods.supportsOneBudANC) pods.setOneBudANC(!pods.oneBudANC)
      else if (key === "e") pods.cycleEarDetection()
    }

    Flickable {
      id: panelFlick
      anchors.fill: parent
      contentWidth: width
      contentHeight: column.implicitHeight
      clip: true
      boundsBehavior: Flickable.StopAtBounds
      flickableDirection: Flickable.VerticalFlick
      interactive: contentHeight > height
      ScrollBar.vertical: ScrollBar { policy: ScrollBar.AsNeeded }

      Column {
        id: column
        width: panelFlick.width
        spacing: Style.spacing.xxl

        RowLayout {
          width: parent.width
          spacing: Style.spacing.panelGap

          AirPodsIcon {
            // The larger size gives the wide buds enough height against a two-line title.
            iconSize: Style.font.displayLarge
            color: root.inkColor(pods.hasAirPods ? root.inks.hero : root.inks.quietHero)
            variant: root.podsVariant
          }

          ColumnLayout {
            Layout.fillWidth: true
            spacing: Style.spacing.xxs

            Text {
              Layout.fillWidth: true
              textFormat: Text.PlainText
              text: pods.modelName || pods.deviceName || "AirPods"
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.title
              font.bold: true
              elide: Text.ElideRight
            }

            Text {
              id: phrase
              Layout.fillWidth: true
              textFormat: Text.PlainText
              text: (pods.hasAirPods ? root.heroPhraseText
                : pods.schemaUnsupported ? "Unsupported status schema"
                : pods.daemonReachable ? "Not connected"
                : "librepods is not running").toUpperCase()
              color: root.quiet
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              font.bold: true
              font.letterSpacing: 1.2
              elide: Text.ElideRight
            }
          }
        }

        Text {
          textFormat: Text.PlainText
          // A command failure gets its own field, or the next status read wipes it unread.
          visible: pods.actionStatus !== "" || (pods.lastError !== "" && pods.daemonReachable)
          width: parent.width
          text: pods.actionStatus !== "" ? pods.actionStatus : pods.lastError
          color: root.urgent
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
          wrapMode: Text.WordWrap
        }

        Column {
          visible: pods.hasBattery
          width: parent.width
          spacing: Style.spacing.xl

          PanelSectionHeader {
            text: "BATTERY"
            foreground: root.foreground
            fontFamily: root.fontFamily
          }

          Column {
            width: parent.width
            spacing: Style.spacing.md

            // A Max has one battery and no case, so the pod trio would draw two dashes and a phantom case.
            PodRow {
              colors: root.rowColors
              visible: pods.isHeadset
              width: parent.width
              label: "Headphones"
              pod: ({ level: pods.headsetBattery.level, charging: pods.headsetBattery.charging, inEar: false })
            }
            PodRow { colors: root.rowColors; visible: !pods.isHeadset; width: parent.width; label: "Left"; pod: pods.leftPod }
            PodRow { colors: root.rowColors; visible: !pods.isHeadset; width: parent.width; label: "Right"; pod: pods.rightPod }
            PodRow {
              colors: root.rowColors
              visible: !pods.isHeadset
              width: parent.width
              label: "Case"
              pod: ({ level: pods.caseBattery.level, charging: pods.caseBattery.charging, inEar: false })
              meta: root.lidLabel
            }
          }
        }

        SectionRule {
          visible: pods.hasBattery && pods.hasAirPods
        }

        Column {
          visible: root.modesVisible
          width: parent.width
          spacing: Style.spacing.xl

          PanelSectionHeader {
            text: "LISTENING MODE"
            foreground: root.foreground
            fontFamily: root.fontFamily
          }

          Column {
            width: parent.width
            spacing: Style.spacing.md

            Repeater {
              model: root.modes
              ModeRow {
                colors: root.rowColors
                hasCursor: root.rowHasCursor(rowName)
                onPointed: function (name) { root.focusRow(name) }
                onCursorEntered: function (row) { root.scrollRowIntoView(row) }
                required property var modelData
                width: parent.width
                mode: modelData
                selectedMode: pods.noiseMode
                onActivated: function (mode) { pods.setNoiseMode(mode) }
              }
            }
          }

          SliderRow {
            colors: root.rowColors
            hasCursor: root.rowHasCursor(rowName)
            onPointed: function (name) { root.focusRow(name) }
            onCursorEntered: function (row) { root.scrollRowIntoView(row) }
            noiseLevel: pods.adaptiveNoiseLevel
            stepPercent: root.adaptiveStepPercent
            onAdjusted: function (level) { pods.setAdaptiveNoiseLevel(level) }
            visible: root.adaptiveVisible
            width: parent.width
          }
        }

        SectionRule {
          visible: root.caVisible || root.oneBudVisible
        }

        Column {
          visible: root.caVisible || root.oneBudVisible
          width: parent.width
          spacing: Style.spacing.md

          ToggleRow {
            colors: root.rowColors
            hasCursor: root.rowHasCursor(rowName)
            onPointed: function (name) { root.focusRow(name) }
            onCursorEntered: function (row) { root.scrollRowIntoView(row) }
            busy: pods.busy
            visible: root.caVisible
            width: parent.width
            rowName: "ca"
            label: "Conversation Awareness"
            caption: "Lower the volume when you start talking"
            checked: pods.conversationalAwareness
            onToggled: pods.setConversationalAwareness(!pods.conversationalAwareness)
          }

          ToggleRow {
            colors: root.rowColors
            hasCursor: root.rowHasCursor(rowName)
            onPointed: function (name) { root.focusRow(name) }
            onCursorEntered: function (row) { root.scrollRowIntoView(row) }
            busy: pods.busy
            visible: root.oneBudVisible
            width: parent.width
            rowName: "onebud"
            label: "One-Bud ANC"
            caption: "Keep noise cancellation on with one pod in"
            checked: pods.oneBudANC
            onToggled: pods.setOneBudANC(!pods.oneBudANC)
          }
        }

        SectionRule {
          visible: pods.hasAirPods
        }

        Column {
          visible: pods.hasAirPods
          width: parent.width
          spacing: Style.spacing.md

          ValueRow {
            colors: root.rowColors
            hasCursor: root.rowHasCursor(rowName)
            onPointed: function (name) { root.focusRow(name) }
            onCursorEntered: function (row) { root.scrollRowIntoView(row) }
            width: parent.width
            rowName: "ear"
            onActivated: pods.cycleEarDetection()
            label: "Ear detection"
            value: Model.earDetectionName(pods.earDetectionBehavior)
          }
        }

        Text {
          textFormat: Text.PlainText
          visible: root.guidanceVisible
          width: parent.width
          text: pods.advertHint
            ? "The case battery cannot show while the AirPods are away, because BlueZ is not serving its advertisement monitor. See \"Case battery while away\" in the README."
            : pods.daemonReachable
            ? "Open the case or connect your AirPods to see battery and listening controls."
            : "Start the librepods daemon to see battery and listening controls."
          color: root.quiet
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
          wrapMode: Text.WordWrap
          horizontalAlignment: Text.AlignHCenter
        }
      }
    }

    Timer {
      interval: root.phraseIntervalMs
      running: root.opened && pods.hasAirPods
      repeat: true
      onTriggered: phraseSwap.restart()
    }

    SequentialAnimation {
      id: phraseSwap
      NumberAnimation {
        target: phrase; property: "opacity"
        to: root.inks.phraseOpacity; duration: Style.duration(180); easing.type: Easing.OutQuad
      }
      ScriptAction {
        script: root.phraseIndex = (root.phraseIndex + 1) % root.activePhrases.length
      }
      NumberAnimation {
        target: phrase; property: "opacity"
        to: 1; duration: Style.duration(260); easing.type: Easing.InQuad
      }
    }
  }

  component SectionRule: PanelSeparator {
    foreground: root.separator
    strength: root.separator.a
  }
}
