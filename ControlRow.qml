pragma ComponentBehavior: Bound

import QtQuick
import qs.Commons
import qs.Ui

CursorSurface {
  id: controlRow
  required property string rowName
  required property var colors

  signal pointed(Item row, var mouse)
  signal cursorEntered(CursorSurface row)

  foreground: colors.foreground
  fill: colors.focusFill
  borderSpec: hasCursor ? Border.flat(colors.focusInk, Math.max(Style.spacing.hairline, Style.hoverBorderWidth)) : Border.none()
  onHasCursorChanged: if (hasCursor) cursorEntered(controlRow)
}
