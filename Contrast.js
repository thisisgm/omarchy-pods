// No QML imports, so contrast calculations also run in a plain JS harness.

const TEXT_CONTRAST = 4.5
const FRAME_CONTRAST = 3
const HUE_FLOOR = 0.2
const GLASS_BRIGHTNESS = 0.75
const GLASS_BAR_ALPHA = 0.35
const MAX_TRACK_ALPHA = 0.99
const CONTRAST_STEPS = 18
const QML_ROUNDING_MARGIN = 0.06
// PanelSeparator's stock strength.
const SEPARATOR_ALPHA = 0.12

function inkAlpha(ink, a) {
  return { r: ink.r, g: ink.g, b: ink.b, a: a }
}

function grayInk(value) {
  return { r: value, g: value, b: value, a: 1 }
}

function compositeInk(ink, ground) {
  return {
    r: ink.r * ink.a + ground.r * (1 - ink.a),
    g: ink.g * ink.a + ground.g * (1 - ink.a),
    b: ink.b * ink.a + ground.b * (1 - ink.a),
    a: 1
  }
}

function linearChannel(value) {
  return value <= 0.04045 ? value / 12.92 : Math.pow((value + 0.055) / 1.055, 2.4)
}

function inkLuminance(ink) {
  return 0.2126 * linearChannel(ink.r) + 0.7152 * linearChannel(ink.g) + 0.0722 * linearChannel(ink.b)
}

function contrastRatio(first, second) {
  const a = inkLuminance(first)
  const b = inkLuminance(second)
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05)
}

function contrastGrounds(background, brightness) {
  return [compositeInk(background, grayInk(0)), compositeInk(background, grayInk(brightness))]
}

function worstContrast(ink, grounds) {
  let least = Infinity
  let lightInk = false
  let darkInk = false
  for (const ground of grounds) {
    const painted = compositeInk(ink, ground)
    lightInk = lightInk || inkLuminance(painted) > inkLuminance(ground)
    darkInk = darkInk || inkLuminance(painted) < inkLuminance(ground)
    least = Math.min(least, contrastRatio(painted, ground))
  }
  // When the ink changes polarity, an intervening backdrop makes it disappear.
  return lightInk && darkInk ? 1 : least
}

// The margin covers the 8-bit framebuffer, so a drawn ink never lands under its floor.
function meetsFloor(ink, grounds, floor) {
  return worstContrast(ink, grounds) >= floor + QML_ROUNDING_MARGIN
}

function mixInk(from, to, portion) {
  return {
    r: from.r + (to.r - from.r) * portion,
    g: from.g + (to.g - from.g) * portion,
    b: from.b + (to.b - from.b) * portion,
    a: from.a + (to.a - from.a) * portion
  }
}

function approachContrast(ink, target, grounds, floor) {
  let low = 0
  let high = 1
  for (let step = 0; step < CONTRAST_STEPS; step++) {
    const midpoint = (low + high) / 2
    if (meetsFloor(mixInk(ink, target, midpoint), grounds, floor)) high = midpoint
    else low = midpoint
  }
  return mixInk(ink, target, high)
}

function liftInk(ink, grounds, floor, maxAlpha = 1) {
  if (ink.a > maxAlpha) ink = inkAlpha(ink, maxAlpha)
  if (meetsFloor(ink, grounds, floor)) return ink

  const dark = inkAlpha(grayInk(0), maxAlpha)
  const light = inkAlpha(grayInk(1), maxAlpha)
  const extreme = worstContrast(dark, grounds) >= worstContrast(light, grounds) ? dark : light
  const originalAlpha = inkAlpha(extreme, ink.a)
  if (meetsFloor(originalAlpha, grounds, floor))
    return approachContrast(ink, originalAlpha, grounds, floor)
  const originalHue = inkAlpha(ink, maxAlpha)
  if (meetsFloor(originalHue, grounds, floor))
    return approachContrast(ink, originalHue, grounds, floor)
  if (meetsFloor(extreme, grounds, floor))
    return approachContrast(ink, extreme, grounds, floor)
  return extreme
}

function dimInk(primary, opacity, grounds, floor) {
  const dim = inkAlpha(primary, primary.a * opacity)
  if (meetsFloor(dim, grounds, floor)) return dim
  if (meetsFloor(primary, grounds, floor)) return approachContrast(dim, primary, grounds, floor)
  return primary
}

function huedInk(ink, fallback) {
  const high = Math.max(ink.r, ink.g, ink.b)
  const low = Math.min(ink.r, ink.g, ink.b)
  return high > 0 && (high - low) / high > HUE_FLOOR ? ink : fallback
}

function panelInks(palette, opacity) {
  // The popup can be unblurred, so use the full white backdrop even on glass.
  const grounds = contrastGrounds(palette.background, 1)
  const focused = grounds.map(ground => compositeInk(palette.focusFill, ground))
  const rowGrounds = grounds.concat(focused)
  const text = liftInk(palette.foreground, rowGrounds, TEXT_CONTRAST)
  const quiet = dimInk(text, opacity, rowGrounds, TEXT_CONTRAST)
  const quietest = dimInk(quiet, 0, rowGrounds, TEXT_CONTRAST)
  const frame = liftInk(palette.foreground, rowGrounds, FRAME_CONTRAST)
  return {
    text: text,
    quiet: quiet,
    phraseOpacity: quietest.a / quiet.a,
    error: liftInk(palette.urgent, grounds, TEXT_CONTRAST),
    hero: frame,
    quietHero: dimInk(frame, opacity, rowGrounds, FRAME_CONTRAST),
    meterFill: liftInk(palette.foreground, grounds, FRAME_CONTRAST),
    lowBattery: liftInk(palette.urgent, grounds, FRAME_CONTRAST),
    meterTrack: liftInk(palette.track, grounds, FRAME_CONTRAST),
    sliderTrack: liftInk(palette.track, rowGrounds, FRAME_CONTRAST, MAX_TRACK_ALPHA),
    sliderFill: text,
    toggleOff: liftInk(palette.toggleOff, rowGrounds, FRAME_CONTRAST),
    toggleOn: liftInk(palette.toggleOn, rowGrounds, FRAME_CONTRAST),
    toggleOffKnob: liftInk(palette.foreground, rowGrounds.map(ground => compositeInk(palette.offFill, ground)), FRAME_CONTRAST),
    toggleOnKnob: liftInk(palette.accent, rowGrounds.map(ground => compositeInk(palette.onFill, ground)), FRAME_CONTRAST),
    separator: liftInk(inkAlpha(palette.foreground, SEPARATOR_ALPHA), grounds, FRAME_CONTRAST),
    focus: liftInk(palette.focus, rowGrounds, FRAME_CONTRAST),
    glyph: frame
  }
}

function barContrastGrounds(foreground, background, themeForeground, themeBackground, mode, blurred) {
  if (mode === "none") {
    // The shell supplies the sampled ink choice, but cannot supply the wallpaper sample.
    const textLuminance = inkLuminance(themeForeground)
    const backgroundLuminance = inkLuminance(themeBackground)
    const crossover = Math.sqrt((textLuminance + 0.05) * (backgroundLuminance + 0.05)) - 0.05
    const gray = grayInk(crossover <= 0.0031308
      ? 12.92 * crossover
      : 1.055 * Math.pow(crossover, 1 / 2.4) - 0.055)
    return inkLuminance(foreground) > crossover ? [grayInk(0), gray] : [gray, grayInk(1)]
  }
  const glassy = mode === "glassy"
  const alpha = glassy ? Math.min(background.a, GLASS_BAR_ALPHA) : background.a
  return contrastGrounds(inkAlpha(background, alpha), glassy && blurred ? GLASS_BRIGHTNESS : 1)
}

function barInks(foreground, background, themeForeground, themeBackground, mode, blurred, opacity) {
  const grounds = barContrastGrounds(foreground, background, themeForeground, themeBackground, mode, blurred)
  const mark = liftInk(foreground, grounds, FRAME_CONTRAST)
  return { mark: mark, quiet: dimInk(mark, opacity, grounds, FRAME_CONTRAST) }
}
