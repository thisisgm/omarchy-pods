pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Layouts
import qs.Commons

ControlRow {
  id: toggleRow
  property string label: ""
  property string caption: ""
  property bool checked: false
  required property bool busy

  signal toggled()

  implicitHeight: Math.max(toggleContent.implicitHeight, toggleSwitch.implicitHeight) + Style.spacing.rowPaddingX

  MouseArea {
    anchors.fill: parent
    hoverEnabled: true
    cursorShape: Qt.PointingHandCursor
    onPositionChanged: function (mouse) { toggleRow.pointed(toggleRow, mouse) }
    onPressed: toggleRow.pointed(toggleRow, null)
    onClicked: if (!toggleRow.busy) toggleRow.toggled()
  }

  RowLayout {
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.verticalCenter: parent.verticalCenter
    anchors.leftMargin: Style.spacing.controlPaddingX
    anchors.rightMargin: Style.spacing.controlPaddingX
    spacing: Style.spacing.rowGap

    ColumnLayout {
      id: toggleContent
      Layout.fillWidth: true
      spacing: Style.spacing.hairline

      Text {
        textFormat: Text.PlainText
        Layout.fillWidth: true
        text: toggleRow.label
        color: toggleRow.colors.foreground
        font.family: Style.font.family
        font.pixelSize: Style.font.body
        elide: Text.ElideRight
      }

      Text {
        textFormat: Text.PlainText
        Layout.fillWidth: true
        text: toggleRow.caption
        color: toggleRow.colors.quiet
        font.family: Style.font.family
        font.pixelSize: Style.font.caption
        elide: Text.ElideRight
      }
    }

    SwitchMark {
      id: toggleSwitch
      Layout.alignment: Qt.AlignVCenter
      checked: toggleRow.checked
      colors: toggleRow.colors
    }
  }

  // ToggleSwitch fixes its knob colors and border alpha internally.
  component SwitchMark: Rectangle {
    id: mark
    required property var colors
    property bool checked: false
    readonly property bool rounded: Style.cornerRadius > 0
    // ToggleSwitch's stock geometry, as fractions of the control height.
    readonly property real trackHeightRatio: 0.55
    readonly property real trackAspect: 1.9
    readonly property real knobRatio: 0.72
    readonly property real knobSize: Math.max(Style.spacing.md, Math.round(height * knobRatio))
    readonly property real knobInset: Math.max(Style.spacing.hairline, Math.round((height - knobSize) / 2))
    readonly property bool mirrored: LayoutMirroring.enabled

    implicitHeight: Math.max(Style.space(22), Math.round(Style.spacing.controlHeight * trackHeightRatio))
    implicitWidth: Math.round(implicitHeight * trackAspect)
    radius: rounded ? height / 2 : 0
    color: checked ? mark.colors.trackFill : mark.colors.toggleOffFill
    border.width: Style.spacing.hairline
    border.color: checked ? colors.toggleOn : colors.toggleOff

    Rectangle {
      width: mark.knobSize
      height: width
      anchors.verticalCenter: parent.verticalCenter
      x: mark.checked !== mark.mirrored ? mark.width - width - mark.knobInset : mark.knobInset
      radius: mark.rounded ? width / 2 : 0
      color: mark.checked ? mark.colors.toggleOnKnob : mark.colors.toggleOffKnob
      Behavior on x {
        NumberAnimation { duration: Style.duration(120); easing.type: Easing.OutCubic }
      }
    }
  }
}
