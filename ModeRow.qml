pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Layouts
import qs.Commons
import "Model.js" as Model

ControlRow {
  id: modeRow
  property int mode: 0
  required property int selectedMode

  signal activated(int mode)

  rowName: "mode:" + mode
  readonly property bool selected: selectedMode === mode

  implicitHeight: modeLabel.implicitHeight + Style.spacing.rowPaddingX

  MouseArea {
    anchors.fill: parent
    hoverEnabled: true
    cursorShape: Qt.PointingHandCursor
    onPositionChanged: function (mouse) { modeRow.pointed(modeRow, mouse) }
    onPressed: modeRow.pointed(modeRow, null)
    onClicked: modeRow.activated(modeRow.mode)
  }

  RowLayout {
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.verticalCenter: parent.verticalCenter
    anchors.leftMargin: Style.spacing.controlPaddingX
    anchors.rightMargin: Style.spacing.controlPaddingX
    spacing: Style.spacing.rowGap

    Text {
      textFormat: Text.PlainText
      id: modeLabel
      Layout.fillWidth: true
      text: Model.noiseModeName(modeRow.mode)
      color: modeRow.selected ? modeRow.colors.foreground : modeRow.colors.quiet
      font.family: Style.font.family
      font.pixelSize: Style.font.body
      elide: Text.ElideRight
    }

    Text {
      textFormat: Text.PlainText
      Layout.alignment: Qt.AlignVCenter
      text: Model.GLYPH_CHECK
      color: modeRow.colors.glyph
      font.family: Style.font.family
      font.pixelSize: Style.font.icon
      opacity: modeRow.selected ? 1.0 : 0.0
    }
  }
}
