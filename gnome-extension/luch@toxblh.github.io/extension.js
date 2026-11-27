// Luch — GNOME Shell quick setting for screen mirroring through luchd (D-Bus dev.luch.Luch1).
// SPDX-License-Identifier: GPL-3.0-or-later
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as MessageTray from 'resource:///org/gnome/shell/ui/messageTray.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import {QuickMenuToggle, SystemIndicator} from 'resource:///org/gnome/shell/ui/quickSettings.js';
import VirtualCursor from './cursor.js';
import QuietAsker from './quiet.js';

const IFACE = `<node>
  <interface name="dev.luch.Luch1">
    <method name="Receivers"><arg type="a(sssssqbb)" direction="out"/></method>
    <method name="Refresh"/>
    <method name="Mirror"><arg name="id" type="s" direction="in"/><arg name="source" type="s" direction="in"/></method>
    <method name="StopMirror"><arg name="id" type="s" direction="in"/></method>
    <method name="Disconnect"><arg name="id" type="s" direction="in"/></method>
    <method name="Stats"><arg name="id" type="s" direction="in"/><arg type="s" direction="out"/></method>
    <method name="Known"><arg type="s" direction="out"/></method>
    <method name="Wake"><arg name="id" type="s" direction="in"/><arg type="s" direction="out"/></method>
    <method name="Sleep"><arg name="id" type="s" direction="in"/></method>
    <method name="Cast"><arg name="id" type="s" direction="in"/><arg name="items" type="as" direction="in"/><arg name="options" type="s" direction="in"/><arg type="s" direction="out"/></method>
    <method name="StopCast"><arg name="id" type="s" direction="in"/></method>
    <method name="Present"><arg name="id" type="s" direction="in"/><arg name="file" type="s" direction="in"/><arg type="s" direction="out"/></method>
    <method name="AnswerQuiet"><arg name="hide" type="b" direction="in"/></method>
    <property name="WebUrl" type="s" access="read"/>
    <property name="Quiet" type="s" access="readwrite"/>
    <signal name="ReceiversChanged"/>
    <signal name="QuietAsk"/>
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
    turnOn: name => `Включить ${name}`,
    waking: name => `Включаю ${name}…`,
    audio: 'Только звук (как колонка)',
    sendFile: 'Показать файл…',
    stopCast: 'Закрыть показанное',
    turnOff: 'Выключить ТВ',
    chooseFiles: 'Что показать на ТВ',
    present: 'Презентация…',
    choosePresentation: 'Презентация для показа (PDF, ODP, PPTX)',
    quiet: mode => `Не беспокоить при показе: ${{ask: 'спросить', on: 'да', off: 'нет'}[mode] ?? mode}`,
    quietTitle: 'Скрыть уведомления на время показа?',
    quietBody: 'Их увидят все, кто смотрит на экран. Ответ запомнится до выхода из сеанса.',
    quietHide: 'Скрыть',
    quietKeep: 'Оставить',
    failed: name => `${name}: не получилось`,
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
    turnOn: name => `Turn on ${name}`,
    waking: name => `Turning on ${name}…`,
    audio: 'Sound only (as a speaker)',
    sendFile: 'Show a file…',
    stopCast: 'Close what is shown',
    turnOff: 'Turn the TV off',
    chooseFiles: 'What to show on the TV',
    present: 'Present…',
    choosePresentation: 'A presentation to show (PDF, ODP, PPTX)',
    quiet: mode => `Do not disturb while presenting: ${mode}`,
    quietTitle: 'Hide notifications while presenting?',
    quietBody: 'Everyone looking at the screen would see them. The answer holds until you log out.',
    quietHide: 'Hide them',
    quietKeep: 'Keep them',
    failed: name => `${name}: that did not work`,
};

/** A D-Bus error's own message, without the "GDBus.Error:...: " prefix. */
function reason(e) {
    return String(e?.message ?? e).replace(/^GDBus\.Error:[^ ]+ /, '');
}

/** Files picked in a dialog (zenity, or kdialog), or [] when cancelled or there is no dialog to show. */
function chooseFiles(title, multiple = true) {
    return new Promise(resolve => {
        let argv = null;
        if (GLib.find_program_in_path('zenity'))
            argv = ['zenity', '--file-selection', ...(multiple ? ['--multiple'] : []), '--separator=\n', `--title=${title}`];
        else if (GLib.find_program_in_path('kdialog'))
            argv = ['kdialog', '--getopenfilename', GLib.get_home_dir(), ...(multiple ? ['--multiple'] : []), '--separate-output', '--title', title];
        if (!argv) {
            resolve([]);
            return;
        }
        try {
            const proc = Gio.Subprocess.new(argv, Gio.SubprocessFlags.STDOUT_PIPE);
            proc.communicate_utf8_async(null, null, (p, res) => {
                try {
                    const [, out] = p.communicate_utf8_finish(res);
                    resolve((out ?? '').split('\n').map(l => l.trim()).filter(Boolean));
                } catch (e) {
                    resolve([]);
                }
            });
        } catch (e) {
            logError(e, 'Luch: file dialog');
            resolve([]);
        }
    });
}

/** A notification with buttons, for QuietAsk. */
function askQuestion(title, body, actions) {
    const source = MessageTray.getSystemSource();
    const n = new MessageTray.Notification({source, title, body, gicon: ICON,
        urgency: MessageTray.Urgency.HIGH, isTransient: true});
    for (const [label, fn] of actions)
        n.addAction(label, fn);
    source.addNotification(n);
}

const LuchToggle = GObject.registerClass(
class LuchToggle extends QuickMenuToggle {
    _init(onMirroringChanged) {
        super._init({title: T.title, gicon: ICON, toggleMode: false});
        this._onMirroringChanged = onMirroringChanged;
        this._rows = [];
        this._asleep = []; // known receivers that are off and can be woken: [id, name]
        this._waking = new Set();
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
            this._quiet = new QuietAsker(proxy, askQuestion, T);
            this._quietId = proxy.connectSignal('QuietAsk', () => this._quiet.onAsk());
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
        try {
            const [json] = await this._proxy.KnownAsync();
            this._asleep = JSON.parse(json)
                .filter(r => !r.online && r.wake?.length > 0)
                .map(r => [r.id, r.name]);
        } catch (e) {
            this._asleep = []; // an older luchd
        }
        this._render(null);
    }

    _render(note) {
        const mirroring = this._mirroring;
        this.checked = mirroring.length > 0;
        this._onMirroringChanged(this.checked);

        this._section.removeAll();
        if (note || (this._rows.length === 0 && this._asleep.length === 0)) {
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
            // A refusal from luchd (an older receiver, the accessibility switch off...) is shown, not only logged.
            const add = (label, fn) => sub.menu.addAction(label, () => fn().catch(e => {
                logError(e, 'Luch');
                Main.notify(T.failed(name), reason(e));
            }));
            if (isMirroring) {
                add(T.stop, () => this._proxy.StopMirrorAsync(id));
            } else {
                add(T.fast, () => this._mirror(id, 'fast'));
                add(T.monitor, () => this._mirror(id, 'monitor'));
                add(T.window, () => this._mirror(id, 'window'));
                add(T.extend, () => this._mirror(id, 'virtual'));
                add(T.audio, () => this._mirror(id, 'audio'));
            }
            add(T.sendFile, async () => {
                const files = await chooseFiles(T.chooseFiles);
                if (files.length > 0)
                    await this._proxy.CastAsync(id, files, '');
            });
            add(T.present, async () => {
                const [file] = await chooseFiles(T.choosePresentation, false);
                if (!file)
                    return;
                const [url] = await this._proxy.PresentAsync(id, file);
                Gio.AppInfo.launch_default_for_uri(url, null);
            });
            if (connected)
                add(T.stopCast, () => this._proxy.StopCastAsync(id));
            add(T.turnOff, () => this._proxy.SleepAsync(id));
            if (connected)
                add(T.disconnect, () => this._proxy.DisconnectAsync(id));
            this._section.addMenuItem(sub);
        }

        // Receivers that are off: one tap turns them on (Wake-on-LAN or the remote's Bluetooth signal).
        for (const [id, name] of this._asleep) {
            const waking = this._waking.has(id);
            const item = new PopupMenu.PopupImageMenuItem(
                waking ? T.waking(name) : T.turnOn(name), 'system-shutdown-symbolic');
            item.setSensitive(!waking);
            item.connect('activate', () => this._wake(id));
            this._section.addMenuItem(item);
        }

        // Do not disturb while presenting or mirroring: ask → on → off.
        const mode = this._proxy?.Quiet;
        if (mode) {
            const next = {ask: 'on', on: 'off', off: 'ask'}[mode] ?? 'ask';
            const item = new PopupMenu.PopupImageMenuItem(T.quiet(mode), 'notifications-disabled-symbolic');
            item.connect('activate', () => {
                this._proxy.Quiet = next;
            });
            this._section.addMenuItem(item);
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

    async _wake(id) {
        this._waking.add(id);
        this._render(null);
        try {
            await this._proxy.WakeAsync(id);
        } catch (e) {
            logError(e, 'Luch: wake');
        }
        // It shows up through ReceiversChanged once it is on the LAN; until then the item says it is waking.
        GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, 60, () => {
            this._waking.delete(id);
            if (this._proxy)
                this._refresh();
            return GLib.SOURCE_REMOVE;
        });
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
        if (this._quietId)
            this._proxy.disconnectSignal(this._quietId);
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
