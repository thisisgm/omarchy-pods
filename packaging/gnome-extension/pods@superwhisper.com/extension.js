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
const BATTERY_TRACK_WIDTH = 64;
const MODES = [
    ['Off', 'off', 'supports_noise_off', 0],
    ['ANC', 'anc', 'supports_noise_control', 1],
    ['Trans.', 'transparency', 'supports_noise_control', 2],
    ['Adapt.', 'adaptive', 'supports_adaptive', 3],
];
const BATTERIES = [['left', 'L'], ['right', 'R'], ['case', 'Case'], ['headset', 'Headset']];

export default class PodsExtension extends Extension {
    enable() {
        this._signals = [];
        this._indicator = new PanelMenu.Button(0, 'Pods');
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
        Main.panel.addToStatusArea(this.uuid, this._indicator);

        this._watch = Gio.bus_watch_name(Gio.BusType.SESSION, BUS_NAME, Gio.BusNameWatcherFlags.NONE,
            (bus, _name, owner) => this._appeared(bus, owner), () => this._clearOwner());
    }

    disable() {
        Gio.bus_unwatch_name(this._watch);
        this._clearOwner();
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
        this._batteryRow = null;
        this._modeRow = null;
        this._adaptiveRow = null;
        this._adaptiveLabel = null;
        this._slider = null;
    }

    _connect(actor, signal, callback) {
        this._signals.push([actor, actor.connect(signal, callback)]);
    }

    _row(child) {
        const row = new PopupMenu.PopupBaseMenuItem({activate: false, can_focus: false, hover: false});
        child.x_expand = true;
        row.add_child(child);
        this._indicator.menu.addMenuItem(row);
        return row;
    }

    _buildMenu() {
        const header = new St.BoxLayout({style_class: 'pods-header'});
        this._headerIcon = new St.Icon({icon_size: 32});
        this._device = new St.Label({style_class: 'pods-device'});
        this._connection = new St.Label();
        const details = new St.BoxLayout({
            layout_manager: new Clutter.BoxLayout({orientation: Clutter.Orientation.VERTICAL}),
        });
        details.add_child(this._device);
        details.add_child(this._connection);
        header.add_child(this._headerIcon);
        header.add_child(details);
        this._row(header);

        const batteryStrip = new St.BoxLayout({style_class: 'pods-batteries'});
        this._batteries = BATTERIES.map(([key, label]) => {
            const column = new St.BoxLayout({
                layout_manager: new Clutter.BoxLayout({orientation: Clutter.Orientation.VERTICAL}),
                x_expand: true,
            });
            const percent = new St.Label();
            const progress = new St.Bin({style_class: 'pods-progress', x_align: Clutter.ActorAlign.START});
            column.add_child(new St.Label({text: label}));
            column.add_child(percent);
            column.add_child(new St.Bin({style_class: 'pods-track', child: progress}));
            batteryStrip.add_child(column);
            return {key, column, percent, progress};
        });
        this._batteryRow = this._row(batteryStrip);

        const modes = new St.BoxLayout({style_class: 'pods-modes'});
        this._modes = MODES.map(([label, word, capability, value]) => {
            const button = new St.Button({label, style_class: 'button pods-mode', can_focus: true, x_expand: true});
            this._connect(button, 'clicked', () => this._set('SetNoiseMode', '(s)', word));
            modes.add_child(button);
            return {button, capability, value};
        });
        this._modeRow = this._row(modes);

        const adaptive = new St.BoxLayout({
            layout_manager: new Clutter.BoxLayout({orientation: Clutter.Orientation.VERTICAL}),
        });
        this._adaptiveLabel = new St.Label();
        this._slider = new Slider(0);
        this._slider.accessible_name = 'Adaptive Noise';
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
        adaptive.add_child(this._adaptiveLabel);
        adaptive.add_child(this._slider);
        this._adaptiveRow = this._row(adaptive);

        this._switches = [
            this._switch('Conversation Awareness', 'conversational_awareness', 'supports_conversational_awareness',
                enabled => this._set('SetConversationalAwareness', '(b)', enabled)),
            this._switch('Ear Detection', 'ear_detection_behavior', null,
                enabled => this._set('SetEarDetection', '(s)', enabled ? 'one' : 'off')),
            this._switch('One-Bud ANC', 'one_bud_anc_mode', 'supports_one_bud_anc',
                enabled => this._set('SetOneBudAnc', '(b)', enabled)),
        ];
    }

    _switch(label, key, capability, action) {
        const item = new PopupMenu.PopupSwitchMenuItem(label, false);
        this._connect(item, 'toggled', (_item, enabled) => action(enabled));
        this._indicator.menu.addMenuItem(item);
        return {item, key, capability};
    }

    _cancelSlider() {
        if (this._sliderTimer)
            GLib.source_remove(this._sliderTimer);
        this._sliderTimer = 0;
    }

    _showSlider() {
        this._adaptiveLabel.text = `Adaptive Noise (${Math.round(this._slider.value * 100)}%)`;
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

    _render(state) {
        const available = BATTERIES.some(([key]) => state[key]?.available);
        this._indicator.visible = state.connected || available;
        if (!this._indicator.visible) {
            this._indicator.menu.close();
            return;
        }
        const variant = state.is_headset ? 'max' : state.is_pro_series ? 'pro' : 'buds';
        this._panelIcon.gicon = this._headerIcon.gicon = this._icons[variant];
        const buds = (state.is_headset ? [state.headset] : [state.left, state.right])
            .filter(cell => cell?.available);
        this._percentage.visible = buds.length > 0;
        this._percentage.text = buds.length > 0 ? `${Math.min(...buds.map(cell => cell.level))}%` : '';
        this._device.text = state.device_name || 'AirPods';
        const worn = state.left?.in_ear || state.right?.in_ear;
        this._connection.text = state.connected ? (worn ? 'Connected · Worn' : 'Connected') : 'Away';
        this._batteryRow.visible = available;
        for (const {key, column, percent, progress} of this._batteries) {
            const cell = state[key];
            column.visible = !!cell?.available;
            if (column.visible) {
                percent.text = `${cell.level}%${cell.charging ? '+' : ''}`;
                progress.width = BATTERY_TRACK_WIDTH * cell.level / 100;
            }
        }
        for (const {button, capability, value} of this._modes) {
            button.visible = !!(state.connected && state.supports_noise_control && state[capability]);
            button.checked = state.noise_mode === value;
        }
        this._modeRow.visible = this._modes.some(({button}) => button.visible);
        this._adaptiveRow.visible = !!(state.connected && state.supports_adaptive && state.noise_mode === 3);
        if (!this._adaptiveRow.visible)
            this._cancelSlider();
        if (this._adaptiveRow.visible && !this._dragging && !this._sliderTimer) {
            this._rendering = true;
            this._slider.value = state.adaptive_noise_level / 100;
            this._showSlider();
            this._rendering = false;
        }
        for (const {item, key, capability} of this._switches) {
            item.visible = !!(state.connected && (capability ? state[capability] : state.model_int > 0));
            if (item.visible)
                item.setToggleState(capability ? state[key] : state[key] !== 2);
        }
    }
}
