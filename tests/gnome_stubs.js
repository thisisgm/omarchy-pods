// Minimal GNOME Shell 46-51 stubs for loading packaging/gnome-extension verbatim under Deno.

const state = {
    nextSignal: 1,
    nextTimer: 1,
    nextWatch: 1,
    nextSub: 1,
    statusArea: new Map(),
    watches: new Map(),
    timers: new Map(),
    subscriptions: new Map(),
    calls: [],
    actors: new Set(),
};

class SignalEmitter {
    constructor() {
        this._handlers = new Map();
        this.visible = true;
        this.destroyed = false;
        state.actors.add(this);
    }

    connect(signal, callback) {
        const id = state.nextSignal++;
        this._handlers.set(id, {signal, callback});
        return id;
    }

    disconnect(id) {
        if (!this._handlers.delete(id))
            throw new Error(`disconnecting unknown signal id ${id}`);
    }

    emit(signal, ...args) {
        for (const handler of [...this._handlers.values()]) {
            if (handler.signal === signal)
                handler.callback(this, ...args);
        }
    }

    show() {
        this.visible = true;
    }

    hide() {
        this.visible = false;
    }

    destroy() {
        this.destroyed = true;
    }
}

class Container extends SignalEmitter {
    constructor(props = {}) {
        super();
        this.children = [];
        Object.assign(this, props);
    }

    add_child(child) {
        this.children.push(child);
    }
}

class BoxLayout extends Container {
    constructor(props = {}) {
        // GNOME 50/51 removed St.BoxLayout's vertical property; reject it here.
        if ('vertical' in props)
            throw new Error('St.BoxLayout({vertical}) is removed in GNOME 50/51; use Clutter.BoxLayout');
        super(props);
    }
}

class Icon extends SignalEmitter {
    constructor(props = {}) {
        super();
        this.gicon = null;
        Object.assign(this, props);
    }
}

class Label extends SignalEmitter {
    constructor(props = {}) {
        super();
        this.text = '';
        Object.assign(this, props);
    }
}

class Bin extends SignalEmitter {
    constructor(props = {}) {
        super();
        this.width = 0;
        Object.assign(this, props);
    }
}

class ButtonActor extends SignalEmitter {
    constructor(props = {}) {
        super();
        this.checked = false;
        Object.assign(this, props);
    }
}

export class Slider extends SignalEmitter {
    constructor(value = 0) {
        super();
        this._value = value;
    }

    get value() {
        return this._value;
    }

    set value(next) {
        this._value = next;
        this.emit('notify::value');
    }
}

class Menu {
    constructor() {
        this.items = [];
        this.closedCount = 0;
    }

    addMenuItem(item) {
        this.items.push(item);
    }

    close() {
        this.closedCount++;
    }
}

export class Button extends Container {
    constructor(alignment, name) {
        super();
        this.alignment = alignment;
        this.name = name;
        this.menu = new Menu();
    }
}

export class PopupBaseMenuItem extends Container {
    constructor(props = {}) {
        super(props);
    }
}

export class PopupSwitchMenuItem extends SignalEmitter {
    constructor(label, active) {
        super();
        this.label = label;
        this.state = !!active;
    }

    get actor() {
        // GNOME 46-51 PopupSwitchMenuItem subclasses St.BoxLayout directly and has no .actor.
        throw new TypeError('PopupSwitchMenuItem has no .actor property in GNOME 46-51');
    }

    setToggleState(active) {
        this.state = !!active;
    }
}

export class Extension {
    constructor(metadata) {
        this.metadata = metadata;
        this.uuid = metadata.uuid;
        this.dir = metadata.dir;
    }
}

export const panel = {
    addToStatusArea(uuid, indicator) {
        state.statusArea.set(uuid, indicator);
    },
};

class Variant {
    constructor(signature, value) {
        this.signature = signature;
        this._value = value;
    }

    unpack() {
        return this._value;
    }

    deepUnpack() {
        return this._value;
    }

    get_child_value(index) {
        return this._value[index];
    }

    get_variant() {
        return this._value;
    }
}

class Cancellable {
    constructor() {
        this.cancelled = false;
    }

    cancel() {
        this.cancelled = true;
    }
}

class FileIcon {
    constructor({file}) {
        if (!file?.path)
            throw new Error('Gio.FileIcon requires a file');
        Deno.statSync(file.path);
        this.file = file;
    }
}

class FakeBus {
    signal_subscribe(sender, iface, member, path, arg0, _flags, callback) {
        const id = state.nextSub++;
        state.subscriptions.set(id, {sender, iface, member, path, arg0, callback});
        return id;
    }

    signal_unsubscribe(id) {
        if (!state.subscriptions.delete(id))
            throw new Error(`unsubscribing unknown bus signal ${id}`);
    }

    call(dest, path, iface, method, parameters, _replyType, _flags, timeoutMs, cancellable, callback) {
        state.calls.push({
            dest,
            path,
            iface,
            method,
            signature: parameters?.signature ?? null,
            args: parameters?.unpack() ?? null,
            timeoutMs,
            cancellable,
            callback,
            bus: this,
        });
    }

    call_finish(result) {
        if (result.error)
            throw result.error;
        return result.reply;
    }
}

const Clutter = {
    ActorAlign: {CENTER: 'center', START: 'start'},
    Orientation: {VERTICAL: 'vertical'},
    BoxLayout: class ClutterBoxLayout {
        constructor(props = {}) {
            Object.assign(this, props);
        }
    },
};

const GLib = {
    PRIORITY_DEFAULT: 0,
    SOURCE_REMOVE: false,
    Variant,
    timeout_add(_priority, delayMs, callback) {
        const id = state.nextTimer++;
        state.timers.set(id, {delayMs, callback});
        return id;
    },
    source_remove(id) {
        if (!state.timers.delete(id))
            throw new Error(`removing unknown timer ${id}`);
        return true;
    },
};

const Gio = {
    BusType: {SESSION: 'session'},
    BusNameWatcherFlags: {NONE: 0},
    DBusSignalFlags: {NONE: 0},
    DBusCallFlags: {NONE: 0},
    Cancellable,
    FileIcon,
    bus_watch_name(busType, name, flags, onAppeared, onVanished) {
        const id = state.nextWatch++;
        state.watches.set(id, {busType, name, flags, onAppeared, onVanished});
        return id;
    },
    bus_unwatch_name(id) {
        if (!state.watches.delete(id))
            throw new Error(`unwatching unknown bus watch ${id}`);
    },
};

const St = {
    BoxLayout,
    Icon,
    Label,
    Bin,
    Button: ButtonActor,
};

export const gi = {Clutter, Gio, GLib, St};

export function reset() {
    state.nextSignal = 1;
    state.nextTimer = 1;
    state.nextWatch = 1;
    state.nextSub = 1;
    state.statusArea.clear();
    state.watches.clear();
    state.timers.clear();
    state.subscriptions.clear();
    state.calls.length = 0;
    state.actors.clear();
}

export const harness = {
    state,
    makeDir(baseUrl) {
        return {
            get_child(name) {
                return {name, path: new URL(name, baseUrl)};
            },
        };
    },
    appear(owner = ':1.42') {
        const bus = new FakeBus();
        for (const watch of state.watches.values())
            watch.onAppeared(bus, watch.name, owner);
        return bus;
    },
    vanish() {
        for (const watch of state.watches.values())
            watch.onVanished();
    },
    emitStatus(statusObj) {
        const json = typeof statusObj === 'string' ? statusObj : JSON.stringify(statusObj);
        const params = new Variant('(sa{sv}as)', [
            'com.superwhisper.Pods1',
            {StatusJson: new Variant('s', json)},
            [],
        ]);
        for (const sub of [...state.subscriptions.values()])
            sub.callback(null, sub.sender, sub.path, sub.iface, sub.member, params);
    },
    resolveGet(call, statusObj) {
        const json = typeof statusObj === 'string' ? statusObj : JSON.stringify(statusObj);
        const reply = new Variant('(v)', [new Variant('v', new Variant('s', json))]);
        call.callback(call.bus, {reply});
    },
    resolveVoid(call) {
        call.callback(call.bus, {reply: new Variant('()', [])});
    },
    rejectCall(call, message) {
        call.callback(call.bus, {error: new Error(message)});
    },
    runTimers() {
        for (const [id, timer] of [...state.timers.entries()]) {
            state.timers.delete(id);
            timer.callback();
        }
    },
    connectedSignalCount() {
        let count = 0;
        for (const actor of state.actors)
            count += actor._handlers.size;
        return count;
    },
};
