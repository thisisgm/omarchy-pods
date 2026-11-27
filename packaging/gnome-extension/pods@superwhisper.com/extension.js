import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import St from 'gi://St';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import {Slider} from 'resource:///org/gnome/shell/ui/slider.js';

const BUS_NAME = 'com.superwhisper.Pods';
const OBJECT_PATH = '/com/superwhisper/Pods';
const PODS = 'com.superwhisper.Pods1';
const PROPERTIES = 'org.freedesktop.DBus.Properties';
const REPLY_TIMEOUT_MS = 5000;
const SLIDER_DELAY_MS = 180;
const METER_WIDTH = 148;
const HEADSET_METER_WIDTH = 180;
const HEADSET_CHARGING_METER_WIDTH = 112;
const LOW_BATTERY_PERCENT = 20;
const MODES = [
    ['Off', 'off', 'supports_noise_off', 0],
    ['Transparency', 'transparency', 'supports_noise_control', 2],
    ['Adaptive', 'adaptive', 'supports_adaptive', 3],
    ['Noise Cancellation', 'anc', 'supports_noise_control', 1],
];
const BATTERIES = [
    ['left', 'Left'],
    ['right', 'Right'],
    ['case', 'Case'],
    ['headset', 'Headphones'],
];
const EAR_MODES = [
    ['Pause when one is out', 'one'],
    ['Pause when both are out', 'both'],
    ['Never pause', 'off'],
];
const LID_LABELS = ['Open', 'Closed'];

function vbox(props = {}) {
    return new St.BoxLayout({
        layout_manager: new Clutter.BoxLayout({orientation: Clutter.Orientation.VERTICAL}),
        ...props,
    });
}

function podMeta(key, cell, lidState) {
    if (key === 'case')
        return LID_LABELS[lidState] ?? (cell?.charging ? 'Charging' : '');
    if (cell?.charging)
        return 'Charging';
    if (cell?.in_ear)
        return 'In ear';
    return '';
}

export default class PodsExtension extends Extension {
    enable() {
        this._signals = [];
        this._indicator = new PanelMenu.Button(0, 'Pods');
        this._indicator.menu.box.add_style_class_name('pods-menu');
        this._indicator.hide();
        this._icons = {};
        for (const variant of ['buds', 'pro', 'max'])
            this._icons[variant] = new Gio.FileIcon({file: this.dir.get_child(`pods-${variant}-symbolic.svg`)});
        this._panelIcon = new St.Icon({style_class: 'system-status-icon'});
        this._percentage = new St.Label({y_align: Clutter.ActorAlign.CENTER});
        const panelBox = new St.BoxLayout({style_class: 'pods-panel'});
        panelBox.add_child(this._panelIcon);
        panelBox.add_child(this._percentage);
        this._indicator.add_child(panelBox);
        this._buildMenu();
        this._connect(this._device, 'style-changed', () => {
            if (this._status)
                this._render(this._status);
        });
        Main.panel.addToStatusArea(this.uuid, this._indicator);

        this._watch = Gio.bus_watch_name(Gio.BusType.SESSION, BUS_NAME, Gio.BusNameWatcherFlags.NONE,
            (bus, _name, owner) => this._appeared(bus, owner), () => this._clearOwner());
    }

    disable() {
        Gio.bus_unwatch_name(this._watch);
        this._watch = 0;
        this._clearOwner();
        this._rendering = false;
        for (const [actor, id] of this._signals)
            actor.disconnect(id);
        this._signals = null;
        this._indicator.destroy();
        this._indicator = null;
        this._icons = null;
        this._switches = null;
        this._batteries = null;
        this._modes = null;
        this._panelIcon = null;
        this._percentage = null;
        this._headerIcon = null;
        this._device = null;
        this._connection = null;
        this._batterySection = null;
        this._batterySep = null;
        this._modeHeader = null;
        this._modeRows = null;
        this._adaptiveRow = null;
        this._adaptivePercent = null;
        this._slider = null;
        this._switchSep = null;
        this._earSep = null;
        this._earButton = null;
        this._earRow = null;
        this._earValue = null;
    }

    _connect(actor, signal, callback) {
        this._signals.push([actor, actor.connect(signal, callback)]);
    }

    _row(child, styleClass = 'pods-item') {
        const row = new PopupMenu.PopupBaseMenuItem({
            activate: false,
            can_focus: false,
            hover: false,
            style_class: styleClass,
        });
        child.x_expand = true;
        row.add_child(child);
        this._indicator.menu.addMenuItem(row);
        return row;
    }

    _separator() {
        const sep = new PopupMenu.PopupSeparatorMenuItem();
        sep.add_style_class_name('pods-separator');
        this._indicator.menu.addMenuItem(sep);
        return sep;
    }

    _buildMenu() {
        const header = new St.BoxLayout({style_class: 'pods-header'});
        this._headerIcon = new St.Icon({icon_size: 36, style_class: 'pods-hero-icon'});
        this._device = new St.Label({style_class: 'pods-device'});
        this._connection = new St.Label({style_class: 'pods-subtitle'});
        const details = vbox({
            style_class: 'pods-header-text',
            x_expand: true,
            y_align: Clutter.ActorAlign.CENTER,
        });
        details.add_child(this._device);
        details.add_child(this._connection);
        header.add_child(this._headerIcon);
        header.add_child(details);
        this._row(header, 'pods-header-row');

        const batteryBox = vbox({style_class: 'pods-section'});
        batteryBox.add_child(new St.Label({text: 'BATTERY', style_class: 'pods-section-title'}));
        this._batteries = BATTERIES.map(([key, label]) => {
            const line = new St.BoxLayout({style_class: 'pods-battery-row', y_align: Clutter.ActorAlign.CENTER});
            const name = new St.Label({
                text: label,
                style_class: key === 'headset' ? 'pods-battery-label pods-battery-wide' : 'pods-battery-label',
                y_align: Clutter.ActorAlign.CENTER,
            });
            const fill = new St.Bin({style_class: 'pods-meter-fill'});
            const track = new St.BoxLayout({
                style_class: 'pods-meter-track',
                x_align: Clutter.ActorAlign.START,
                y_align: Clutter.ActorAlign.CENTER,
            });
            track.add_child(fill);
            const percent = new St.Label({style_class: 'pods-battery-pct', y_align: Clutter.ActorAlign.CENTER});
            const meta = new St.Label({style_class: 'pods-battery-meta', y_align: Clutter.ActorAlign.CENTER});
            line.add_child(name);
            line.add_child(track);
            line.add_child(percent);
            line.add_child(meta);
            batteryBox.add_child(line);
            return {key, line, track, fill, percent, meta};
        });
        this._batterySection = this._row(batteryBox, 'pods-section-row');
        this._batterySep = this._separator();

        const modeHeading = new St.Label({text: 'LISTENING MODE', style_class: 'pods-section-title'});
        this._modeHeader = this._row(modeHeading, 'pods-section-head-row');
        this._modeRows = [];
        this._modes = MODES.map(([label, word, capability, value]) => {
            const content = new St.BoxLayout({style_class: 'pods-mode-content', x_expand: true});
            const text = new St.Label({
                text: label,
                style_class: 'pods-mode-label',
                x_expand: true,
                y_align: Clutter.ActorAlign.CENTER,
            });
            const check = new St.Icon({
                icon_name: 'object-select-symbolic',
                style_class: 'pods-check',
                y_align: Clutter.ActorAlign.CENTER,
            });
            content.add_child(text);
            content.add_child(check);
            const button = new St.Button({
                child: content,
                style_class: 'pods-row-button',
                can_focus: true,
                x_expand: true,
            });
            this._connect(button, 'clicked', () => this._set('SetNoiseMode', '(s)', word));
            const row = this._row(button, 'pods-compact-row');
            this._modeRows.push(row);
            return {row, button, text, check, capability, value};
        });

        const adaptive = vbox({style_class: 'pods-adaptive-box'});
        const adaptiveHead = new St.BoxLayout({style_class: 'pods-adaptive-head'});
        adaptiveHead.add_child(new St.Label({
            text: 'Adaptive noise',
            style_class: 'pods-adaptive-title',
            x_expand: true,
            y_align: Clutter.ActorAlign.CENTER,
        }));
        this._adaptivePercent = new St.Label({
            style_class: 'pods-adaptive-pct',
            y_align: Clutter.ActorAlign.CENTER,
        });
        adaptiveHead.add_child(this._adaptivePercent);
        this._slider = new Slider(0);
        this._slider.accessible_name = 'Adaptive noise';
        this._connect(this._slider, 'drag-begin', () => {
            this._dragging = true;
            this._cancelSlider();
        });
        this._connect(this._slider, 'drag-end', () => {
            this._dragging = false;
            this._sendSlider();
        });
        this._connect(this._slider, 'notify::value', () => {
            this._showSlider();
            if (this._rendering || this._dragging)
                return;
            this._cancelSlider();
            this._sliderTimer = GLib.timeout_add(GLib.PRIORITY_DEFAULT, SLIDER_DELAY_MS, () => {
                this._sliderTimer = 0;
                this._sendSlider();
                return GLib.SOURCE_REMOVE;
            });
        });
        adaptive.add_child(adaptiveHead);
        adaptive.add_child(this._slider);
        this._adaptiveRow = this._row(adaptive, 'pods-adaptive-row');

        this._switchSep = this._separator();
        this._switches = [
            this._switch('Conversation Awareness', 'Lower the volume when you start talking',
                'conversational_awareness', 'supports_conversational_awareness',
                enabled => this._set('SetConversationalAwareness', '(b)', enabled)),
            this._switch('One-Bud ANC', 'Keep noise cancellation on with one pod in',
                'one_bud_anc_mode', 'supports_one_bud_anc',
                enabled => this._set('SetOneBudAnc', '(b)', enabled)),
        ];

        this._earSep = this._separator();
        const earContent = new St.BoxLayout({style_class: 'pods-ear-content', x_expand: true});
        earContent.add_child(new St.Label({
            text: 'Ear detection',
            style_class: 'pods-ear-title',
            x_expand: true,
            y_align: Clutter.ActorAlign.CENTER,
        }));
        this._earValue = new St.Label({
            style_class: 'pods-ear-value',
            y_align: Clutter.ActorAlign.CENTER,
        });
        earContent.add_child(this._earValue);
        const earButton = new St.Button({
            child: earContent,
            style_class: 'pods-row-button',
            can_focus: true,
            x_expand: true,
        });
        this._connect(earButton, 'clicked', () => {
            const current = Number.isInteger(this._status?.ear_detection_behavior)
                ? this._status.ear_detection_behavior
                : 0;
            const next = (current + 1) % EAR_MODES.length;
            this._set('SetEarDetection', '(s)', EAR_MODES[next][1]);
        });
        this._earButton = earButton;
        this._earRow = this._row(earButton, 'pods-compact-row');
    }

    _switch(title, caption, key, capability, action) {
        const box = new St.BoxLayout({style_class: 'pods-toggle-content', x_expand: true});
        const labels = vbox({style_class: 'pods-toggle-labels', x_expand: true, y_align: Clutter.ActorAlign.CENTER});
        labels.add_child(new St.Label({text: title, style_class: 'pods-toggle-title'}));
        labels.add_child(new St.Label({text: caption, style_class: 'pods-toggle-caption'}));
        const toggle = new PopupMenu.Switch(false);
        toggle.y_align = Clutter.ActorAlign.CENTER;
        box.add_child(labels);
        box.add_child(toggle);
        const button = new St.Button({
            child: box,
            style_class: 'pods-row-button pods-toggle-button',
            can_focus: true,
            x_expand: true,
        });
        this._connect(button, 'clicked', () => {
            const next = !toggle.state;
            toggle.state = next;
            action(next);
        });
        const row = this._row(button, 'pods-compact-row');
        return {row, button, toggle, key, capability};
    }

    _cancelSlider() {
        if (this._sliderTimer)
            GLib.source_remove(this._sliderTimer);
        this._sliderTimer = 0;
    }

    _showSlider() {
        this._adaptivePercent.text = `${Math.round(this._slider.value * 100)}%`;
    }

    _sendSlider() {
        if (this._adaptiveRow.visible)
            this._set('SetAdaptiveLevel', '(y)', Math.round(this._slider.value * 100));
    }

    _clearOwner() {
        this._cancelSlider();
        this._dragging = false;
        this._status = null;
        this._owner?.cancellable.cancel();
        if (this._owner)
            this._owner.bus.signal_unsubscribe(this._owner.signal);
        this._owner = null;
        this._indicator.menu.close();
        this._indicator.hide();
    }

    _appeared(bus, name) {
        this._clearOwner();
        const owner = {bus, name, cancellable: new Gio.Cancellable(), changed: false};
        this._owner = owner;
        owner.signal = bus.signal_subscribe(name, PROPERTIES, 'PropertiesChanged', OBJECT_PATH,
            PODS, Gio.DBusSignalFlags.NONE, (_bus, _sender, _path, _interface, _signal, parameters) => {
                if (this._owner !== owner)
                    return;
                const [, changed] = parameters.deepUnpack();
                if (changed.StatusJson) {
                    owner.changed = true;
                    this._accept(changed.StatusJson.unpack());
                }
            });
        this._call(PROPERTIES, 'Get', new GLib.Variant('(ss)', [PODS, 'StatusJson']), reply => {
            if (!owner.changed)
                this._accept(reply.get_child_value(0).get_variant().unpack());
        });
    }

    _call(iface, method, parameters, onReply) {
        const owner = this._owner;
        if (!owner)
            return;
        owner.bus.call(owner.name, OBJECT_PATH, iface, method, parameters, null, Gio.DBusCallFlags.NONE,
            REPLY_TIMEOUT_MS, owner.cancellable, (bus, result) => {
                try {
                    const reply = bus.call_finish(result);
                    if (this._owner === owner)
                        onReply?.(reply);
                } catch (error) {
                    if (this._owner === owner) {
                        console.error(`Pods: ${method}: ${error.message}`);
                        if (iface === PODS && this._status)
                            this._render(this._status);
                    }
                }
            });
    }

    _set(method, signature, value) {
        this._call(PODS, method, new GLib.Variant(signature, [value]));
    }

    // Sample input: {"schema_version":1,"connected":false,"case":{"available":true,"level":64,"charging":false}}.
    _accept(json) {
        try {
            const state = JSON.parse(json);
            if (state?.schema_version !== 1)
                throw new Error('unsupported status schema');
            this._status = state;
            this._render(state);
        } catch (error) {
            console.error(`Pods: invalid status: ${error.message}`);
            this._status = null;
            this._indicator.menu.close();
            this._indicator.hide();
        }
    }

    _meterFillStyle() {
        const node = this._device?.peek_theme_node?.();
        const fg = node?.get_foreground_color?.();
        return fg ? `background-color: rgba(${fg.red}, ${fg.green}, ${fg.blue}, 0.88);` : null;
    }

    _render(state) {
        const available = BATTERIES.some(([key]) => state[key]?.available);
        this._indicator.visible = state.connected || available;
        if (!this._indicator.visible) {
            this._indicator.menu.close();
            return;
        }
        const variant = state.is_headset ? 'max' : state.is_pro_series ? 'pro' : 'buds';
        this._panelIcon.gicon = this._headerIcon.gicon = this._icons[variant];
        this._panelIcon.opacity = state.connected ? 255 : 150;
        this._headerIcon.opacity = state.connected ? 255 : 120;
        const buds = (state.is_headset ? [state.headset] : [state.left, state.right])
            .filter(cell => cell?.available);
        this._percentage.visible = buds.length > 0;
        this._percentage.text = buds.length > 0 ? `${Math.min(...buds.map(cell => cell.level))}%` : '';
        this._device.text = state.model_name || state.device_name || 'AirPods';
        this._connection.text = state.connected ? 'CONNECTED' : 'NOT CONNECTED';

        this._batterySection.visible = available;
        const fillStyle = this._meterFillStyle();
        for (const {key, line, track, fill, percent, meta} of this._batteries) {
            const cell = state[key];
            const show = state.is_headset ? key === 'headset' && !!cell?.available : key !== 'headset' && !!cell?.available;
            line.visible = show;
            if (show) {
                const level = Math.max(0, Math.min(100, cell.level));
                const metaText = podMeta(key, cell, state.lid_state);
                const showMeta = key !== 'headset' || metaText.length > 0;
                const width = key === 'headset'
                    ? (showMeta ? HEADSET_CHARGING_METER_WIDTH : HEADSET_METER_WIDTH)
                    : METER_WIDTH;
                percent.text = `${level}%`;
                track.width = width;
                fill.width = Math.round(width * level / 100);
                const low = level <= LOW_BATTERY_PERCENT && !cell.charging;
                if (low) {
                    fill.style = null;
                    fill.add_style_class_name('pods-meter-low');
                } else {
                    fill.remove_style_class_name('pods-meter-low');
                    fill.style = fillStyle;
                }
                meta.visible = showMeta;
                meta.text = metaText;
            }
        }

        let anyMode = false;
        for (const {row, button, text, check, capability, value} of this._modes) {
            const show = !!(state.connected && state.supports_noise_control && state[capability]);
            row.visible = show;
            if (show)
                anyMode = true;
            const active = state.noise_mode === value;
            check.visible = active;
            if (active) {
                button.add_style_class_name('pods-mode-selected');
                text.add_style_class_name('pods-mode-active');
            } else {
                button.remove_style_class_name('pods-mode-selected');
                text.remove_style_class_name('pods-mode-active');
            }
        }
        this._modeHeader.visible = anyMode;
        this._adaptiveRow.visible = !!(state.connected && state.supports_adaptive && state.noise_mode === 3);
        if (!this._adaptiveRow.visible)
            this._cancelSlider();
        if (this._adaptiveRow.visible && !this._dragging && !this._sliderTimer) {
            this._rendering = true;
            this._slider.value = state.adaptive_noise_level / 100;
            this._showSlider();
            this._rendering = false;
        }

        let anySwitch = false;
        for (const {row, toggle, key, capability} of this._switches) {
            const show = !!(state.connected && state[capability]);
            row.visible = show;
            if (show) {
                anySwitch = true;
                toggle.state = !!state[key];
            }
        }

        const showEar = !!(state.connected && state.model_int > 0);
        this._earRow.visible = showEar;
        if (showEar) {
            const entry = EAR_MODES[state.ear_detection_behavior];
            this._earValue.text = entry ? entry[0] : 'Unknown';
        }

        this._batterySep.visible = available && (anyMode || anySwitch || showEar);
        this._switchSep.visible = anyMode && anySwitch;
        this._earSep.visible = (anyMode || anySwitch) && showEar;
    }
}
