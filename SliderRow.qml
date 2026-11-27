pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Layouts
import qs.Commons
import qs.Commons as Commons
import qs.Ui

ControlRow {
  id: sliderRow
  required property int noiseLevel
  required property int stepPercent

  signal adjusted(real level)

  readonly property int adaptiveTickCount: 5
  rowName: "adaptive"
  implicitHeight: sliderColumn.implicitHeight + Style.spacing.controlGap

  HoverHandler {
    onHoveredChanged: if (hovered) sliderRow.pointed(sliderRow.rowName)
  }

  TapHandler {
    onPressedChanged: if (pressed) sliderRow.pointed(sliderRow.rowName)
  }

  // PanelSlider reads its knob border from its bar argument in Omarchy 4.5.
  QtObject {
    id: sliderPalette
    readonly property color background: Commons.Color.popups.background
  }

  Column {
    id: sliderColumn
    anchors.left: parent.left
    anchors.right: parent.right
    anchors.verticalCenter: parent.verticalCenter
    anchors.leftMargin: Style.spacing.controlPaddingX
    anchors.rightMargin: Style.spacing.controlPaddingX
    spacing: Style.spacing.labelGap

    RowLayout {
      width: parent.width
      spacing: Style.spacing.rowGap

      Text {
        Layout.fillWidth: true
        textFormat: Text.PlainText
        text: "Adaptive noise"
        color: sliderRow.colors.quiet
        font.family: Style.font.family
        font.pixelSize: Style.font.bodySmall
      }

      Text {
        textFormat: Text.PlainText
        text: sliderRow.noiseLevel + "%"
        color: sliderRow.colors.foreground
        font.family: Style.font.family
        font.pixelSize: Style.font.bodySmall
      }
    }

    PanelSlider {
      width: parent.width
      bar: sliderPalette
      trackColor: sliderRow.colors.sliderTrack
      fillColor: sliderRow.colors.sliderFill
      knobColor: fillColor
      minimum: 0
      maximum: 100
      step: sliderRow.stepPercent
      integer: true
      tickCount: sliderPalette.background.a === 1 ? sliderRow.adaptiveTickCount : 0
      value: sliderRow.noiseLevel
      onReleased: function (v) { sliderRow.adjusted(v) }
    }
  }
}
