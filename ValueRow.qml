pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Layouts
import qs.Commons

ControlRow {
  id: valueRow
  property string label: ""
  property string value: ""

  signal activated()

  implicitHeight: valueLabel.implicitHeight + Style.spacing.rowPaddingX

  MouseArea {
    anchors.fill: parent
    hoverEnabled: true
    cursorShape: Qt.PointingHandCursor
    onPositionChanged: function (mouse) { valueRow.pointed(valueRow, mouse) }
    onPressed: valueRow.pointed(valueRow, null)
    onClicked: valueRow.activated()
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
      id: valueLabel
      Layout.fillWidth: true
      text: valueRow.label
      color: valueRow.colors.foreground
      font.family: Style.font.family
      font.pixelSize: Style.font.body
      elide: Text.ElideRight
    }

    Text {
      textFormat: Text.PlainText
      text: valueRow.value
      color: valueRow.colors.quiet
      font.family: Style.font.family
      font.pixelSize: Style.font.bodySmall
      elide: Text.ElideRight
    }
  }
}
