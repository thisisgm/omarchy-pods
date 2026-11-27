pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Layouts
import qs.Commons
import "Model.js" as Model

Item {
  id: podRow
  required property var colors
  readonly property int lowBatteryPercent: 20
  property string label: ""
  property var pod: Model.defaultPod()
  property string meta: ""

  readonly property string metaText: meta !== "" ? meta : Model.podMeta(pod)
  readonly property bool low: pod.level !== Model.LEVEL_UNKNOWN
    && pod.level <= lowBatteryPercent && !pod.charging

  implicitHeight: podLayout.implicitHeight

  RowLayout {
    id: podLayout
    anchors.left: parent.left
    anchors.right: parent.right
    spacing: Style.spacing.rowGap

    Text {
      textFormat: Text.PlainText
      text: podRow.label
      color: podRow.colors.quiet
      font.family: Style.font.family
      font.pixelSize: Style.font.bodySmall
      // Style.space(44) was cut for Left, Right and Case, so a longer label takes the room it needs.
      Layout.preferredWidth: Math.max(Style.space(44), implicitWidth + Style.spacing.controlPaddingX)
    }

    Rectangle {
      id: meterTrack
      Layout.fillWidth: true
      Layout.alignment: Qt.AlignVCenter
      implicitHeight: Style.spacing.md
      radius: height / 2
      color: podRow.colors.trackFill
      border.width: Style.spacing.hairline
      border.color: podRow.colors.meterTrack

      Rectangle {
        width: meterTrack.width * Model.levelFraction(podRow.pod.level)
        height: parent.height
        radius: parent.radius
        color: podRow.low ? podRow.colors.lowBattery : podRow.colors.meterFill
      }
    }

    Text {
      textFormat: Text.PlainText
      text: Model.levelText(podRow.pod.level)
      color: podRow.colors.foreground
      font.family: Style.font.family
      font.pixelSize: Style.font.bodySmall
      horizontalAlignment: Text.AlignRight
      Layout.preferredWidth: Style.space(38)
    }

    Text {
      textFormat: Text.PlainText
      text: podRow.metaText
      color: podRow.colors.quiet
      font.family: Style.font.family
      font.pixelSize: Style.font.caption
      elide: Text.ElideRight
      Layout.preferredWidth: Style.space(56)
    }
  }
}
