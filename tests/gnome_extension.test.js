import PodsExtension from '../packaging/gnome-extension/pods@superwhisper.com/extension.js';
import {harness, reset} from './gnome_stubs.js';

const iconsDir = new URL('../packaging/icons/', import.meta.url);

function assert(condition, message) {
    if (!condition)
        throw new Error(message);
}

function assertEq(actual, expected, label) {
    if (JSON.stringify(actual) !== JSON.stringify(expected))
        throw new Error(`${label}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
}

function makeStatus(overrides = {}) {
    return {
        schema_version: 1,
        connected: true,
        device_name: 'GM AirPods Pro',
        model_int: 12,
        is_pro_series: true,
        is_headset: false,
        supports_noise_control: true,
        supports_noise_off: true,
        supports_adaptive: true,
        supports_conversational_awareness: true,
        supports_one_bud_anc: true,
        noise_mode: 1,
        adaptive_noise_level: 50,
        conversational_awareness: false,
        ear_detection_behavior: 1,
        one_bud_anc_mode: false,
        left: {available: true, level: 60, charging: false, in_ear: true},
        right: {available: true, level: 80, charging: true, in_ear: false},
        case: {available: true, level: 90, charging: false},
        headset: {available: false, level: 0, charging: false},
        ...overrides,
    };
}

function enableExtension() {
    reset();
    const ext = new PodsExtension({
        uuid: 'pods@superwhisper.com',
        dir: harness.makeDir(iconsDir),
    });
    ext.enable();
    return ext;
}

function menuPieces(ext) {
    const [headerRow, batteryRow, modeRow, adaptiveRow, caSwitch, earSwitch, oneBudSwitch] =
        ext._indicator.menu.items;
    const modeButtons = modeRow.children[0].children;
    const adaptiveBox = adaptiveRow.children[0];
    const [adaptiveLabel, slider] = adaptiveBox.children;
    return {
        headerRow,
        batteryRow,
        modeRow,
        modeButtons,
        adaptiveRow,
        adaptiveLabel,
        slider,
        caSwitch,
        earSwitch,
        oneBudSwitch,
    };
}

Deno.test('Pro 2 gating: Adaptive slider hidden outside Adaptive mode, shown in Adaptive, teardown clean', () => {
    const ext = enableExtension();
    assertEq(harness.state.watches.size, 1, 'bus watch count');
    assert(!ext._indicator.visible, 'indicator starts hidden');

    harness.appear(':1.90');
    assertEq(harness.state.subscriptions.size, 1, 'PropertiesChanged subscription');
    assertEq(harness.state.calls.length, 1, 'initial Get call');
    assertEq(harness.state.calls[0].method, 'Get', 'initial D-Bus method');
    assertEq(harness.state.calls[0].args, ['com.superwhisper.Pods1', 'StatusJson'], 'Get args');

    // Deliver Pro 2 in ANC (noise_mode = 1).
    harness.resolveGet(harness.state.calls[0], makeStatus({noise_mode: 1, adaptive_noise_level: 50}));
    const ui = menuPieces(ext);
    assert(ext._indicator.visible, 'indicator visible when connected');
    assertEq(ext._panelIcon.gicon.file.name, 'pods-pro-symbolic.svg', 'pro icon');
    assertEq(ext._percentage.text, '60%', 'lowest bud percentage');
    assertEq(ext._device.text, 'GM AirPods Pro', 'device name');
    assertEq(ext._connection.text, 'Connected · Worn', 'worn status');
    assert(ui.modeRow.visible, 'mode row visible');
    assertEq(ui.modeButtons.map(b => [b.label, b.visible, b.checked]), [
        ['Off', true, false],
        ['ANC', true, true],
        ['Trans.', true, false],
        ['Adapt.', true, false],
    ], 'mode buttons in ANC');
    assert(!ui.adaptiveRow.visible, 'Adaptive slider must stay hidden while ANC is active');
    assert(ui.caSwitch.visible && !ui.caSwitch.state, 'CA switch visible and off');
    assert(ui.earSwitch.visible && ui.earSwitch.state, 'Ear Detection switch visible and on');
    assert(ui.oneBudSwitch.visible && !ui.oneBudSwitch.state, 'One-Bud ANC switch visible and off');

    // Clicking Adapt. sends SetNoiseMode("adaptive").
    ui.modeButtons[3].emit('clicked');
    const modeCall = harness.state.calls.at(-1);
    assertEq(modeCall.iface, 'com.superwhisper.Pods1', 'SetNoiseMode iface');
    assertEq(modeCall.method, 'SetNoiseMode', 'SetNoiseMode method');
    assertEq(modeCall.signature, '(s)', 'SetNoiseMode sig');
    assertEq(modeCall.args, ['adaptive'], 'SetNoiseMode arg');
    harness.resolveVoid(modeCall);

    // Daemon emits PropertiesChanged with noise_mode = 3 (Adaptive).
    harness.emitStatus(makeStatus({noise_mode: 3, adaptive_noise_level: 50}));
    assert(ui.adaptiveRow.visible, 'Adaptive slider visible in Adaptive mode');
    assertEq(ui.adaptiveLabel.text, 'Adaptive Noise (50%)', 'initial Adaptive label');
    assertEq(ui.slider.value, 0.5, 'initial slider value');

    // Drag slider to 75%: no timer during drag, sends SetAdaptiveLevel(75) on drag-end.
    const callsBeforeDrag = harness.state.calls.length;
    ui.slider.emit('drag-begin');
    ui.slider.value = 0.75;
    assertEq(ui.adaptiveLabel.text, 'Adaptive Noise (75%)', 'live label during drag');
    assertEq(harness.state.timers.size, 0, 'no debounce timer while dragging');
    assertEq(harness.state.calls.length, callsBeforeDrag, 'no D-Bus call mid-drag');
    ui.slider.emit('drag-end');
    const dragCall = harness.state.calls.at(-1);
    assertEq(dragCall.method, 'SetAdaptiveLevel', 'drag-end method');
    assertEq(dragCall.signature, '(y)', 'drag-end sig');
    assertEq(dragCall.args, [75], 'drag-end level');

    // Non-drag slider change schedules a 180 ms debounce timer; leaving Adaptive cancels it.
    ui.slider.value = 0.25;
    assertEq(harness.state.timers.size, 1, 'debounce timer scheduled');
    const callsBeforeLeave = harness.state.calls.length;
    harness.emitStatus(makeStatus({noise_mode: 1, adaptive_noise_level: 75}));
    assert(!ui.adaptiveRow.visible, 'Adaptive slider hidden again after leaving Adaptive');
    assertEq(harness.state.timers.size, 0, 'pending slider timer cancelled on mode change');
    harness.runTimers();
    assertEq(harness.state.calls.length, callsBeforeLeave, 'no refused SetAdaptiveLevel sent after leaving Adaptive');

    // Re-enter Adaptive and let non-drag debounce fire.
    harness.emitStatus(makeStatus({noise_mode: 3, adaptive_noise_level: 75}));
    ui.slider.value = 0.30;
    assertEq(harness.state.timers.size, 1, 'debounce timer armed');
    harness.runTimers();
    const debounceCall = harness.state.calls.at(-1);
    assertEq(debounceCall.method, 'SetAdaptiveLevel', 'debounced method');
    assertEq(debounceCall.args, [30], 'debounced level');

    // Toggle switches send expected D-Bus methods and signatures.
    ui.caSwitch.emit('toggled', true);
    assertEq(harness.state.calls.at(-1).method, 'SetConversationalAwareness', 'CA method');
    assertEq(harness.state.calls.at(-1).args, [true], 'CA arg');
    ui.earSwitch.emit('toggled', false);
    assertEq(harness.state.calls.at(-1).method, 'SetEarDetection', 'Ear method');
    assertEq(harness.state.calls.at(-1).args, ['off'], 'Ear arg');
    ui.oneBudSwitch.emit('toggled', true);
    assertEq(harness.state.calls.at(-1).method, 'SetOneBudAnc', 'One-Bud method');
    assertEq(harness.state.calls.at(-1).args, [true], 'One-Bud arg');

    // Failed setter re-renders authoritative daemon state (reverting optimistic toggle).
    ui.oneBudSwitch.setToggleState(true);
    const prevError = console.error;
    console.error = () => {};
    try {
        harness.rejectCall(harness.state.calls.at(-1), 'refused');
    } finally {
        console.error = prevError;
    }
    assertEq(ui.oneBudSwitch.state, false, 'failed setter reverts switch to last daemon status');

    // Full teardown leaves zero watches, subscriptions, timers, or connected signals.
    const indicator = ext._indicator;
    ext.disable();
    assert(indicator.destroyed, 'indicator destroyed');
    assertEq(harness.state.watches.size, 0, 'watches cleared on disable');
    assertEq(harness.state.subscriptions.size, 0, 'subscriptions cleared on disable');
    assertEq(harness.state.timers.size, 0, 'timers cleared on disable');
    assertEq(harness.connectedSignalCount(), 0, 'all actor signals disconnected on disable');
});

Deno.test('Plain AirPods hide noise modes (including Off), Max uses headset icon, away/empty/invalid states', () => {
    const ext = enableExtension();
    harness.appear(':1.91');

    // PropertiesChanged arriving before Get wins over a stale Get reply.
    harness.emitStatus(makeStatus({
        device_name: 'AirPods',
        model_int: 2,
        is_pro_series: false,
        is_headset: false,
        supports_noise_control: false,
        supports_noise_off: true,
        supports_adaptive: false,
        supports_conversational_awareness: false,
        supports_one_bud_anc: false,
        noise_mode: 0,
        left: {available: true, level: 95, charging: false, in_ear: false},
        right: {available: false, level: 0, charging: false, in_ear: false},
        case: {available: false, level: 0, charging: false},
    }));
    harness.resolveGet(harness.state.calls[0], makeStatus({device_name: 'Stale'}));

    const ui = menuPieces(ext);
    assertEq(ext._device.text, 'AirPods', 'stale Get ignored after PropertiesChanged');
    assertEq(ext._panelIcon.gicon.file.name, 'pods-buds-symbolic.svg', 'buds icon');
    assertEq(ext._connection.text, 'Connected', 'not worn');
    assert(!ui.modeRow.visible, 'plain buds hide the entire mode row including Off');
    assert(!ui.adaptiveRow.visible, 'plain buds hide Adaptive row');
    assert(!ui.caSwitch.visible, 'plain buds hide CA switch');
    assert(!ui.oneBudSwitch.visible, 'plain buds hide One-Bud ANC switch');
    assert(ui.earSwitch.visible, 'plain buds keep Ear Detection switch');

    // AirPods Max uses headset icon and battery.
    harness.emitStatus(makeStatus({
        device_name: 'AirPods Max',
        is_pro_series: false,
        is_headset: true,
        supports_adaptive: false,
        supports_conversational_awareness: false,
        supports_one_bud_anc: false,
        left: {available: false, level: 0, charging: false, in_ear: false},
        right: {available: false, level: 0, charging: false, in_ear: false},
        case: {available: false, level: 0, charging: false},
        headset: {available: true, level: 42, charging: false},
    }));
    assertEq(ext._panelIcon.gicon.file.name, 'pods-max-symbolic.svg', 'max icon');
    assertEq(ext._percentage.text, '42%', 'headset percentage');
    const headsetCol = ext._batteries.find(b => b.key === 'headset');
    assertEq(headsetCol.column.children[2].x_align, 'start', 'battery track anchored flush-left');
    assertEq(headsetCol.progress.width, 27, 'headset progress bar rounded pixel width');

    // Disconnected with case battery stays visible as Away with controls hidden.
    harness.emitStatus(makeStatus({
        connected: false,
        left: {available: false, level: 0, charging: false, in_ear: false},
        right: {available: false, level: 0, charging: false, in_ear: false},
        case: {available: true, level: 64, charging: false},
        headset: {available: false, level: 0, charging: false},
    }));
    assert(ext._indicator.visible, 'away with case battery remains visible');
    assertEq(ext._connection.text, 'Away', 'away label');
    assertEq(ext._percentage.visible, false, 'no bud percentage when only case available');
    assert(!ui.modeRow.visible && !ui.adaptiveRow.visible && !ui.earSwitch.visible, 'controls hidden when away');

    // Disconnected with no available batteries hides indicator.
    harness.emitStatus(makeStatus({
        connected: false,
        left: {available: false, level: 0, charging: false, in_ear: false},
        right: {available: false, level: 0, charging: false, in_ear: false},
        case: {available: false, level: 0, charging: false},
        headset: {available: false, level: 0, charging: false},
    }));
    assert(!ext._indicator.visible, 'empty away state hides indicator');

    // Invalid schema hides indicator without throwing.
    const prevError = console.error;
    console.error = () => {};
    try {
        harness.emitStatus('{"schema_version":99}');
    } finally {
        console.error = prevError;
    }
    assert(!ext._indicator.visible, 'unsupported schema hides indicator');

    // Owner vanish hides indicator and unsubscribes.
    harness.emitStatus(makeStatus());
    assert(ext._indicator.visible, 'visible before vanish');
    harness.vanish();
    assert(!ext._indicator.visible, 'hidden after owner vanish');
    assertEq(harness.state.subscriptions.size, 0, 'signal unsubscribed on vanish');

    ext.disable();
});
