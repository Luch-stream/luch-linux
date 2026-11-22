// Luch — GNOME Shell quick setting for screen mirroring through luchd (D-Bus dev.luch.Luch1).
// SPDX-License-Identifier: GPL-3.0-or-later
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import {QuickMenuToggle, SystemIndicator} from 'resource:///org/gnome/shell/ui/quickSettings.js';
import VirtualCursor from './cursor.js';

const IFACE = `<node>
  <interface name="dev.luch.Luch1">
    <method name="Receivers"><arg type="a(sssssqbb)" direction="out"/></method>
    <method name="Refresh"/>
    <method name="Mirror"><arg name="id" type="s" direction="in"/><arg name="source" type="s" direction="in"/></method>
    <method name="StopMirror"><arg name="id" type="s" direction="in"/></method>
    <method name="Disconnect"><arg name="id" type="s" direction="in"/></method>
    <method name="Stats"><arg name="id" type="s" direction="in"/><arg type="s" direction="out"/></method>
    <property name="WebUrl" type="s" access="read"/>
    <signal name="ReceiversChanged"/>
  </interface>
</node>`;
const LuchProxy = Gio.DBusProxy.makeProxyWrapper(IFACE);

let ICON = null; // Gio.Icon from the extension dir, set in enable()

// ponytail: two-language table instead of gettext; switch to gettext when a third language arrives.
const RU = GLib.get_language_names().some(l => l.startsWith('ru'));
const T = RU ? {
    title: 'Луч',
    header: 'Трансляция на экран',
    noDaemon: 'luchd не запущен',
    searching: 'Ищу приёмники…',
    fast: 'Весь экран — минимальная задержка',
    monitor: 'Экран (через портал)',
    window: 'Окно',
    extend: 'Расширить рабочий стол',
    stop: 'Остановить',
    disconnect: 'Отключиться',
    receivers: n => `Приёмников: ${n}`,
    web: 'Веб-приёмник — откройте в браузере ТВ',
    webOpen: 'Открыть в браузере этого компьютера',
} : {
    title: 'Luch',
    header: 'Screen mirroring',
    noDaemon: 'luchd is not running',
    searching: 'Searching for receivers…',
    fast: 'Full screen — lowest latency',
    monitor: 'Screen (portal)',
    window: 'Window',
    extend: 'Extend desktop',
    stop: 'Stop',
    disconnect: 'Disconnect',
    receivers: n => `${n} receiver${n === 1 ? '' : 's'}`,
    web: 'Web receiver — open in the TV\u2019s browser',
    webOpen: 'Open in this computer\u2019s browser',
};

const LuchToggle = GObject.registerClass(
class LuchToggle extends QuickMenuToggle {
    _init(onMirroringChanged) {
        super._init({title: T.title, gicon: ICON, toggleMode: false});
        this._onMirroringChanged = onMirroringChanged;
        this._rows = [];
        this._lastId = null;
        this._statsTimer = 0;

        this.menu.setHeader(ICON, T.title, T.header);
        this._section = new PopupMenu.PopupMenuSection();
        this.menu.addMenuItem(this._section);

        this.connect('clicked', () => this._onClicked());
        this._menuId = this.menu.connect('open-state-changed', (_menu, open) => {
            if (open)
                this._discover();
        });

        this._proxy = new LuchProxy(Gio.DBus.session, 'dev.luch.Luch1', '/dev/luch/Luch1', (proxy, error) => {
            if (error) {
                logError(error, 'Luch: D-Bus proxy');
                return;
            }
            this._signalId = proxy.connectSignal('ReceiversChanged', () => this._refresh());
            this._ownerId = proxy.connect('notify::g-name-owner', () => this._discover());
            this._propId = proxy.connect('g-properties-changed', () => this._refresh());
            this._discover();
        }, null, Gio.DBusProxyFlags.DO_NOT_AUTO_START);
    }

    get _mirroring() {
        return this._rows.filter(r => r[7]);
    }

    _discover() {
        if (this._proxy?.g_name_owner)
            this._proxy.RefreshAsync().catch(e => logError(e, 'Luch: refresh discovery'));
        this._refresh(); // Show the cache immediately; ReceiversChanged supplies new results.
    }

    async _refresh() {
        if (!this._proxy?.g_name_owner) {
            this._rows = [];
            this._render(T.noDaemon);
            return;
        }
        try {
            [this._rows] = await this._proxy.ReceiversAsync();
        } catch (e) {
            this._rows = [];
        }
        this._render(null);
    }

    _render(note) {
        const mirroring = this._mirroring;
        this.checked = mirroring.length > 0;
        this._onMirroringChanged(this.checked);

        this._section.removeAll();
        if (note || this._rows.length === 0) {
            const item = new PopupMenu.PopupMenuItem(note ?? T.searching, {reactive: false});
            this._section.addMenuItem(item);
        }
        for (const [id, name, model, , , , connected, isMirroring] of this._rows) {
            const sub = new PopupMenu.PopupSubMenuMenuItem(name, true);
            if (isMirroring)
                sub.icon.icon_name = 'media-playback-start-symbolic';
            else
                sub.icon.gicon = ICON;
            if (model)
                sub.label.text = `${name} · ${model}`;
            const add = (label, fn) => sub.menu.addAction(label, () => fn().catch(e => logError(e, 'Luch')));
            if (isMirroring) {
                add(T.stop, () => this._proxy.StopMirrorAsync(id));
            } else {
                add(T.fast, () => this._mirror(id, 'fast'));
                add(T.monitor, () => this._mirror(id, 'monitor'));
                add(T.window, () => this._mirror(id, 'window'));
                add(T.extend, () => this._mirror(id, 'virtual'));
            }
            if (connected)
                add(T.disconnect, () => this._proxy.DisconnectAsync(id));
            this._section.addMenuItem(sub);
        }

        // Built-in web receiver: the URL for the TV; opening it on the PC is a quick test.
        const url = this._proxy?.WebUrl ?? '';
        if (url) {
            const web = new PopupMenu.PopupSubMenuMenuItem(T.web, true);
            web.icon.gicon = ICON;
            web.menu.addAction(T.webOpen, () => {
                try {
                    Gio.AppInfo.launch_default_for_uri(url, null);
                } catch (e) {
                    logError(e, 'Luch');
                }
            });
            const item = new PopupMenu.PopupMenuItem(url, {reactive: false});
            item.label.add_style_class_name('monospace');
            web.menu.addMenuItem(item);
            this._section.addMenuItem(web);
        }

        if (mirroring.length > 0) {
            this.subtitle = mirroring[0][1];
            this._startStats(mirroring[0][0]);
        } else {
            this._stopStats();
            this.subtitle = note ?? (this._rows.length === 1 ? this._rows[0][1] : T.receivers(this._rows.length));
        }
    }

    _mirror(id, source) {
        this._lastId = id;
        return this._proxy.MirrorAsync(id, source);
    }

    _onClicked() {
        const mirroring = this._mirroring;
        if (mirroring.length > 0) {
            for (const r of mirroring)
                this._proxy.StopMirrorAsync(r[0]).catch(e => logError(e, 'Luch'));
            return;
        }
        // One tap = the lowest-latency mode to the last (or only/first) receiver.
        const target = this._rows.find(r => r[0] === this._lastId) ?? this._rows[0];
        if (target)
            this._mirror(target[0], 'fast').catch(e => logError(e, 'Luch'));
        else
            this.menu.open();
    }

    _startStats(id) {
        if (this._statsTimer)
            return;
        this._statsTimer = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, 1, () => {
            this._proxy.StatsAsync(id).then(([json]) => {
                const s = JSON.parse(json);
                const name = this._mirroring[0]?.[1] ?? '';
                if (s.est_ms)
                    this.subtitle = `${name} · ≈${Math.round(s.est_ms)} ms`;
            }).catch(() => {});
            return GLib.SOURCE_CONTINUE;
        });
    }

    _stopStats() {
        if (this._statsTimer)
            GLib.source_remove(this._statsTimer);
        this._statsTimer = 0;
    }

    destroy() {
        this._stopStats();
        if (this._menuId)
            this.menu.disconnect(this._menuId);
        if (this._signalId)
            this._proxy.disconnectSignal(this._signalId);
        if (this._propId)
            this._proxy.disconnect(this._propId);
        if (this._ownerId)
            this._proxy.disconnect(this._ownerId);
        this._proxy = null;
        super.destroy();
    }
});

const LuchIndicator = GObject.registerClass(
class LuchIndicator extends SystemIndicator {
    _init() {
        super._init();
        this._icon = this._addIndicator();
        this._icon.gicon = ICON;
        this._icon.visible = false;
        this.quickSettingsItems.push(new LuchToggle(on => {
            this._icon.visible = on;
        }));
    }

    destroy() {
        this.quickSettingsItems.forEach(item => item.destroy());
        super.destroy();
    }
});

export default class LuchExtension extends Extension {
    enable() {
        ICON = Gio.icon_new_for_string(`${this.path}/luch-symbolic.svg`);
        this._indicator = new LuchIndicator();
        Main.panel.statusArea.quickSettings.addExternalIndicator(this._indicator);
        this._cursor = new VirtualCursor();
    }

    disable() {
        this._cursor?.destroy();
        this._cursor = null;
        this._indicator?.destroy();
        this._indicator = null;
        ICON = null;
    }
}
