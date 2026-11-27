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
        model_name: 'AirPods Pro 2',
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
        ear_detection_behavior: 0,
        one_bud_anc_mode: false,
        lid_state: 2,
        left: {available: true, level: 60, charging: false, in_ear: true},
        right: {available: true, level: 18, charging: false, in_ear: false},
        case: {available: true, level: 90, charging: true},
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

Deno.test('Pro 2 Omarchy layout: modes, Adaptive slider gating, switches, 3-state ear detection, teardown', () => {
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
    assert(ext._indicator.visible, 'indicator visible when connected');
    assertEq(ext._panelIcon.gicon.file.name, 'pods-pro-symbolic.svg', 'pro icon');
    assertEq(ext._percentage.text, '18%', 'lowest bud percentage');
    assertEq(ext._device.text, 'AirPods Pro 2', 'model name preferred in header');
    assertEq(ext._connection.text, 'CONNECTED', 'connected subtitle');
    assert(ext._modeHeader.visible, 'mode header visible');
    assertEq(ext._modes.map(m => [m.text.text, m.row.visible, m.check.visible]), [
        ['Off', true, false],
        ['Transparency', true, false],
        ['Adaptive', true, false],
        ['Noise Cancellation', true, true],
    ], 'Omarchy mode order and active checkmark in ANC');
    assert(!ext._adaptiveRow.visible, 'Adaptive slider must stay hidden while ANC is active');

    // Battery rows show In ear, low-battery class (<=20% uncharged), and Charging metadata.
    const leftRow = ext._batteries.find(b => b.key === 'left');
    const rightRow = ext._batteries.find(b => b.key === 'right');
    const caseRow = ext._batteries.find(b => b.key === 'case');
    assertEq(leftRow.meta.text, 'In ear', 'left meta');
    assert(!leftRow.fill.has_style_class_name('pods-meter-low'), 'left not low');
    assertEq(leftRow.fill.style, 'background-color: rgba(255, 255, 255, 0.88);', 'dark theme foreground fill');
    assert(rightRow.fill.has_style_class_name('pods-meter-low'), 'right <=20% gets low class');
    assertEq(rightRow.fill.style, null, 'low battery uses CSS red class instead of inline foreground');
    assertEq(caseRow.meta.text, 'Charging', 'case charging meta');

    // Theme switch to light foreground updates battery fill dynamically.
    ext._device._fg = {red: 46, green: 52, blue: 54};
    ext._device.emit('style-changed');
    assertEq(leftRow.fill.style, 'background-color: rgba(46, 52, 54, 0.88);', 'light theme foreground fill');

    // Clicking Adaptive sends SetNoiseMode("adaptive").
    ext._modes[2].button.emit('clicked');
    const modeCall = harness.state.calls.at(-1);
    assertEq(modeCall.iface, 'com.superwhisper.Pods1', 'SetNoiseMode iface');
    assertEq(modeCall.method, 'SetNoiseMode', 'SetNoiseMode method');
    assertEq(modeCall.signature, '(s)', 'SetNoiseMode sig');
    assertEq(modeCall.args, ['adaptive'], 'SetNoiseMode arg');
    harness.resolveVoid(modeCall);

    // Daemon emits PropertiesChanged with noise_mode = 3 (Adaptive).
    harness.emitStatus(makeStatus({noise_mode: 3, adaptive_noise_level: 50}));
    assert(ext._adaptiveRow.visible, 'Adaptive slider visible in Adaptive mode');
    assertEq(ext._adaptivePercent.text, '50%', 'initial Adaptive percent');
    assertEq(ext._slider.value, 0.5, 'initial slider value');

    // Drag slider to 75%: no timer during drag, sends SetAdaptiveLevel(75) on drag-end.
    const callsBeforeDrag = harness.state.calls.length;
    ext._slider.emit('drag-begin');
    ext._slider.value = 0.75;
    assertEq(ext._adaptivePercent.text, '75%', 'live percent during drag');
    assertEq(harness.state.timers.size, 0, 'no debounce timer while dragging');
    assertEq(harness.state.calls.length, callsBeforeDrag, 'no D-Bus call mid-drag');
    ext._slider.emit('drag-end');
    const dragCall = harness.state.calls.at(-1);
    assertEq(dragCall.method, 'SetAdaptiveLevel', 'drag-end method');
    assertEq(dragCall.signature, '(y)', 'drag-end sig');
    assertEq(dragCall.args, [75], 'drag-end level');

    // Non-drag slider change schedules a 180 ms debounce timer; leaving Adaptive cancels it.
    ext._slider.value = 0.25;
    assertEq(harness.state.timers.size, 1, 'debounce timer scheduled');
    const callsBeforeLeave = harness.state.calls.length;
    harness.emitStatus(makeStatus({noise_mode: 1, adaptive_noise_level: 75}));
    assert(!ext._adaptiveRow.visible, 'Adaptive slider hidden again after leaving Adaptive');
    assertEq(harness.state.timers.size, 0, 'pending slider timer cancelled on mode change');
    harness.runTimers();
    assertEq(harness.state.calls.length, callsBeforeLeave, 'no refused SetAdaptiveLevel sent after leaving Adaptive');

    // Re-enter Adaptive and let non-drag debounce fire.
    harness.emitStatus(makeStatus({noise_mode: 3, adaptive_noise_level: 75}));
    ext._slider.value = 0.30;
    assertEq(harness.state.timers.size, 1, 'debounce timer armed');
    harness.runTimers();
    const debounceCall = harness.state.calls.at(-1);
    assertEq(debounceCall.method, 'SetAdaptiveLevel', 'debounced method');
    assertEq(debounceCall.args, [30], 'debounced level');

    // Feature toggles and 3-state ear detection cycle.
    const [caSwitch, oneBudSwitch] = ext._switches;
    caSwitch.button.emit('clicked');
    assertEq(harness.state.calls.at(-1).method, 'SetConversationalAwareness', 'CA method');
    assertEq(harness.state.calls.at(-1).args, [true], 'CA arg');

    assertEq(ext._earValue.text, 'Pause when one is out', 'ear 0 label');
    ext._earButton.emit('clicked');
    assertEq(harness.state.calls.at(-1).method, 'SetEarDetection', 'Ear method');
    assertEq(harness.state.calls.at(-1).args, ['both'], 'Ear cycles 0 -> both');
    harness.emitStatus(makeStatus({ear_detection_behavior: 1}));
    assertEq(ext._earValue.text, 'Pause when both are out', 'ear 1 label');
    ext._earButton.emit('clicked');
    assertEq(harness.state.calls.at(-1).args, ['off'], 'Ear cycles 1 -> off');
    harness.emitStatus(makeStatus({ear_detection_behavior: 2}));
    assertEq(ext._earValue.text, 'Never pause', 'ear 2 label');
    ext._earButton.emit('clicked');
    assertEq(harness.state.calls.at(-1).args, ['one'], 'Ear cycles 2 -> one');

    oneBudSwitch.button.emit('clicked');
    assertEq(harness.state.calls.at(-1).method, 'SetOneBudAnc', 'One-Bud method');
    assertEq(harness.state.calls.at(-1).args, [true], 'One-Bud arg');

    // Failed setter re-renders authoritative daemon state (reverting optimistic toggle).
    const prevError = console.error;
    console.error = () => {};
    try {
        harness.rejectCall(harness.state.calls.at(-1), 'refused');
    } finally {
        console.error = prevError;
    }
    assertEq(oneBudSwitch.toggle.state, false, 'failed setter reverts switch to last daemon status');

    // Full teardown leaves zero watches, subscriptions, timers, signals, or actor refs.
    const indicator = ext._indicator;
    ext.disable();
    assert(indicator.destroyed, 'indicator destroyed');
    assertEq(harness.state.watches.size, 0, 'watches cleared on disable');
    assertEq(harness.state.subscriptions.size, 0, 'subscriptions cleared on disable');
    assertEq(harness.state.timers.size, 0, 'timers cleared on disable');
    assertEq(harness.connectedSignalCount(), 0, 'all actor signals disconnected on disable');
    for (const [key, value] of Object.entries(ext)) {
        if (key.startsWith('_'))
            assert(value === null || value === 0 || value === false, `leaked field ${key} after disable`);
    }
});

Deno.test('Plain AirPods hide noise modes (including Off), Max uses Headphones row, away/empty/invalid states', () => {
    const ext = enableExtension();
    harness.appear(':1.91');

    // PropertiesChanged arriving before Get wins over a stale Get reply.
    harness.emitStatus(makeStatus({
        device_name: 'AirPods',
        model_name: '',
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
    harness.resolveGet(harness.state.calls[0], makeStatus({device_name: 'Stale', model_name: 'Stale'}));

    assertEq(ext._device.text, 'AirPods', 'stale Get ignored after PropertiesChanged');
    assertEq(ext._panelIcon.gicon.file.name, 'pods-buds-symbolic.svg', 'buds icon');
    assert(!ext._modeHeader.visible, 'plain buds hide mode header');
    assert(ext._modes.every(m => !m.row.visible), 'plain buds hide all mode rows including Off');
    assert(!ext._adaptiveRow.visible, 'plain buds hide Adaptive row');
    assert(ext._switches.every(s => !s.row.visible), 'plain buds hide Pro switches');
    assert(ext._earRow.visible, 'plain buds keep Ear detection row');

    // AirPods Max uses headset icon and Headphones battery row with flush-left meter.
    harness.emitStatus(makeStatus({
        device_name: 'AirPods Max',
        model_name: 'AirPods Max',
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
    const headsetRow = ext._batteries.find(b => b.key === 'headset');
    assert(headsetRow.line.visible, 'headset row visible');
    assertEq(headsetRow.line.children[1].x_align, 'start', 'meter track anchored flush-left');
    assertEq(headsetRow.fill.width, 77, 'headset meter rounded pixel width (42% of 184)');

    // Disconnected with case battery stays visible as NOT CONNECTED with lid state and controls hidden.
    harness.emitStatus(makeStatus({
        connected: false,
        lid_state: 1,
        left: {available: false, level: 0, charging: false, in_ear: false},
        right: {available: false, level: 0, charging: false, in_ear: false},
        case: {available: true, level: 64, charging: false},
        headset: {available: false, level: 0, charging: false},
    }));
    assert(ext._indicator.visible, 'away with case battery remains visible');
    assertEq(ext._connection.text, 'NOT CONNECTED', 'away subtitle');
    assertEq(ext._headerIcon.opacity, 120, 'dimmed icon when away');
    assertEq(ext._batteries.find(b => b.key === 'case').meta.text, 'Closed', 'lid closed meta');
    assert(!ext._modeHeader.visible && !ext._adaptiveRow.visible && !ext._earRow.visible, 'controls hidden when away');

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
