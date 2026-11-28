// Luch — GNOME Shell quick setting for screen mirroring through luchd (D-Bus dev.luch.Luch1).
// SPDX-License-Identifier: GPL-3.0-or-later
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Clutter from 'gi://Clutter';
import GObject from 'gi://GObject';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {CheckBox} from 'resource:///org/gnome/shell/ui/checkBox.js';
import * as ModalDialog from 'resource:///org/gnome/shell/ui/modalDialog.js';
import * as MessageTray from 'resource:///org/gnome/shell/ui/messageTray.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import {QuickMenuToggle, SystemIndicator} from 'resource:///org/gnome/shell/ui/quickSettings.js';
import VirtualCursor from './cursor.js';
import QuietAsker from './quiet.js';
import Reporter from './report.js';
import {plainProblem} from './plain.js';
import * as Group from './group.js';
import {chooseGroup} from './groupDialog.js';

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
    <method name="Find"><arg name="mode" type="s" direction="in"/></method>
    <method name="Present"><arg name="id" type="s" direction="in"/><arg name="file" type="s" direction="in"/><arg type="s" direction="out"/></method>
    <method name="AnswerQuiet"><arg name="hide" type="b" direction="in"/></method>
    <method name="Pair"><arg name="id" type="s" direction="in"/><arg name="code" type="s" direction="in"/><arg type="s" direction="out"/></method>
    <method name="Pairing"><arg name="id" type="s" direction="in"/><arg type="s" direction="out"/></method>
    <method name="Direct"><arg name="key" type="s" direction="in"/><arg type="s" direction="out"/></method>
    <method name="DirectOff"><arg type="s" direction="out"/></method>
    <method name="DirectStatus"><arg type="s" direction="out"/></method>
    <method name="DirectOffers"><arg type="s" direction="out"/></method>
    <method name="Report"><arg name="text" type="s" direction="in"/><arg name="keep_names" type="b" direction="in"/><arg type="s" direction="out"/></method>
    <method name="SendReport"><arg name="path" type="s" direction="in"/><arg type="s" direction="out"/></method>
    <method name="MirrorGroup"><arg name="ids" type="as" direction="in"/><arg name="roles" type="as" direction="in"/><arg name="source" type="s" direction="in"/><arg name="mode" type="s" direction="in"/></method>
    <method name="AddToGroup"><arg name="member" type="s" direction="in"/><arg name="id" type="s" direction="in"/><arg name="role" type="s" direction="in"/></method>
    <method name="RemoveFromGroup"><arg name="id" type="s" direction="in"/></method>
    <property name="WebUrl" type="s" access="read"/>
    <property name="Quiet" type="s" access="readwrite"/>
    <property name="WatchDevice" type="b" access="readwrite"/>
    <signal name="ReceiversChanged"/>
    <signal name="FindProgress"><arg name="status" type="s"/></signal>
    <signal name="PairChanged"><arg name="id" type="s"/><arg name="state" type="s"/></signal>
    <signal name="DirectChanged"><arg name="status" type="s"/></signal>
    <signal name="QuietAsk"/>
  </interface>
</node>`;
const LuchProxy = Gio.DBusProxy.makeProxyWrapper(IFACE);

let ICON = null; // Gio.Icon from the extension dir, set in enable()

// ponytail: two-language table instead of gettext; switch to gettext when a third language arrives.
const RU = GLib.get_language_names().some(l => l.startsWith('ru'));
const G = Group.words(RU); // the words of showing on several TVs (group.js)
const T = RU ? {
    title: 'Луч',
    header: 'Показ экрана на ТВ',
    noDaemon: 'Служба Луча не запущена',
    startDaemon: 'Запустить службу Луча',
    searching: 'Ищу приёмники…',
    firstRun: 'Откройте Луч на телевизоре: он появится здесь',
    fast: 'Показать экран',
    monitor: 'Показать другой монитор…',
    window: 'Показать одно окно…',
    extend: 'Как второй экран',
    stop: 'Остановить показ',
    stopOn: name => `Остановить показ на ${name}`,
    mirroringNow: 'идёт показ',
    disconnect: 'Отключиться',
    receivers: n => n === 0 ? 'Приёмников нет' : `Приёмников: ${n}`,
    more: 'Ещё',
    web: 'Веб-приёмник: откройте этот адрес в браузере ТВ',
    webOpen: 'Открыть в браузере этого компьютера',
    turnOn: name => `Включить ${name}`,
    waking: name => `Включаю ${name}…`,
    audio: 'Только звук (как колонка)',
    sendFile: 'Показать файл…',
    stopCast: 'Убрать файл с экрана ТВ',
    turnOff: 'Выключить ТВ',
    chooseFiles: 'Что показать на ТВ',
    present: 'Показать презентацию…',
    choosePresentation: 'Презентация для показа (PDF, ODP, PPTX)',
    quiet: 'Не беспокоить при показе:',
    quietModes: {ask: 'Спросить при первом показе', on: 'Всегда', off: 'Никогда'},
    quietTitle: 'Скрыть уведомления на время показа?',
    quietBody: 'Их увидят все, кто смотрит на экран. Ответ запомнится до выхода из сеанса.',
    quietHide: 'Скрыть',
    quietKeep: 'Оставить',
    watchDevice: 'Следить за нагревом и батареей ТВ',
    failed: name => `${name}: не получилось`,
    allowOnTv: name => `${name}: нажмите на ТВ «Разрешить»`,
    codeTitle: name => `Код с экрана ${name}`,
    codeHint: 'Введите цифры, которые сейчас показывает ТВ. Это нужно только в первый раз.',
    pair: 'Подключить',
    cancel: 'Отмена',
    paired: name => `${name}: подключено, код больше не понадобится`,
    findHarder: 'Искать настойчивее (Bluetooth, звук, QR-код)',
    rungs: {mdns: 'сеть (mDNS)', scan: 'все адреса сети', ble: 'Bluetooth',
        sound: 'звук ТВ через микрофон — на ТВ выберите «Сказать адрес звуком»', qr: 'QR-код ТВ через камеру',
        direct: 'ТВ, к которым можно подключиться напрямую'},
    trying: what => `Ищу: ${what}…`,
    direct: 'Подключиться напрямую (Wi-Fi Direct)…',
    directTo: name => `Подключиться к ${name} напрямую (Wi-Fi Direct)…`,
    directTitle: name => name ? `Подключиться к ${name} напрямую?` : 'Подключиться напрямую к ТВ рядом?',
    directBody: net => (net ? `Этот компьютер отключится от «${net}» на время показа и потом вернётся.`
        : 'Wi-Fi этого компьютера подключится к сети ТВ на время показа.') +
        ' ТВ спросит разрешения на своём экране.',
    directGo: 'Подключиться',
    directOff: net => net ? `Отключиться от сети ТВ (вернуться в «${net}»)` : 'Отключиться от сети ТВ',
    directOn: name => `${name}: подключено напрямую`,
    directSteps: {looking: 'ищу ТВ рядом по Bluetooth', opening: 'открываю канал Bluetooth',
        pairing: 'сопряжение', asking: 'разрешите подключение на ТВ', joining: 'подключаюсь к сети ТВ',
        connecting: 'подключаюсь к ТВ'},
    directing: (name, step) => `Напрямую${name ? ` к ${name}` : ''}: ${step}…`,
    report: 'Сообщить о проблеме…',
    reporting: 'Собираю отчёт…',
    reportWhat: 'Что случилось?',
    reportHint: 'Например: с HEVC картинка дёргается',
    reportHide: 'Скрыть адреса и имена',
    reportMake: 'Собрать отчёт',
    reportReady: 'Отчёт готов',
    reportBody: file => `${file} в папке «Загрузки». Ничего не отправлено: перетащите файл в чат или приложите к issue.`,
    reportShow: 'Показать файл',
    reportCopy: 'Скопировать описание',
    reportIssue: 'Открыть issue',
    reportSend: 'Отправить разработчикам',
    reportSending: 'Отправляю отчёт разработчикам…',
    reportSent: n => `Отправлено. Номер отчёта ${n} — назовите его, если будете писать в чат.`,
    reportSentNoFile: n => `Отправлено. Номер отчёта ${n} — назовите его, если будете писать в чат. Сам файл сервер не принял: пришлите его, если попросим.`,
    reportNotSent: e => `Не отправлено: ${reason(e)}. Файл остался в «Загрузках»: его можно показать, перетащить в чат или приложить к issue.`,
    reportCopyNumber: 'Скопировать номер',
    close: 'Закрыть',
} : {
    title: 'Luch',
    header: 'Show this screen on a TV',
    noDaemon: 'Luch\u2019s service is not running',
    startDaemon: 'Start Luch\u2019s service',
    searching: 'Searching for receivers…',
    firstRun: 'Open Luch on the TV: it shows up here',
    fast: 'Mirror the screen',
    monitor: 'Mirror another monitor…',
    window: 'Show one window…',
    extend: 'As a second screen',
    stop: 'Stop mirroring',
    stopOn: name => `Stop mirroring to ${name}`,
    mirroringNow: 'mirroring',
    disconnect: 'Disconnect',
    receivers: n => n === 0 ? 'No receivers' : `${n} receiver${n === 1 ? '' : 's'}`,
    more: 'More',
    web: 'Web receiver: open this address in the TV\u2019s browser',
    webOpen: 'Open in this computer\u2019s browser',
    turnOn: name => `Turn on ${name}`,
    waking: name => `Turning on ${name}…`,
    audio: 'Sound only (as a speaker)',
    sendFile: 'Show a file…',
    stopCast: 'Stop showing the file',
    turnOff: 'Turn the TV off',
    chooseFiles: 'What to show on the TV',
    present: 'Present…',
    choosePresentation: 'A presentation to show (PDF, ODP, PPTX)',
    quiet: 'Do not disturb while presenting:',
    quietModes: {ask: 'Ask the first time', on: 'Always', off: 'Never'},
    quietTitle: 'Hide notifications while presenting?',
    quietBody: 'Everyone looking at the screen would see them. The answer holds until you log out.',
    quietHide: 'Hide them',
    quietKeep: 'Keep them',
    watchDevice: 'Watch the TV\u2019s heat and battery',
    failed: name => `${name}: that did not work`,
    allowOnTv: name => `${name}: press Allow on the TV`,
    codeTitle: name => `The code on ${name}`,
    codeHint: 'Type the digits the TV shows now. This is needed only the first time.',
    pair: 'Connect',
    cancel: 'Cancel',
    paired: name => `${name}: connected, no code next time`,
    findHarder: 'Search harder (Bluetooth, sound, QR code)',
    rungs: {mdns: 'the network (mDNS)', scan: 'every address of the network', ble: 'Bluetooth',
        sound: 'the TV\u2019s sound through the microphone: on the TV, choose \u201cSay the address by sound\u201d',
        qr: 'the TV\u2019s QR code through the camera', direct: 'TVs that connect directly'},
    trying: what => `Looking: ${what}…`,
    direct: 'Connect directly (Wi-Fi Direct)…',
    directTo: name => `Connect directly to ${name} (Wi-Fi Direct)…`,
    directTitle: name => name ? `Connect directly to ${name}?` : 'Connect directly to a TV nearby?',
    directBody: net => (net ? `This computer leaves \u201c${net}\u201d while showing, and comes back after.`
        : 'This computer\u2019s Wi-Fi joins the TV\u2019s network while showing.') +
        ' The TV asks on its screen first.',
    directGo: 'Connect',
    directOff: net => net ? `Leave the TV\u2019s network (back to \u201c${net}\u201d)` : 'Leave the TV\u2019s network',
    directOn: name => `${name}: connected directly`,
    directSteps: {looking: 'looking for TVs nearby over Bluetooth', opening: 'opening the Bluetooth channel',
        pairing: 'pairing', asking: 'allow the connection on the TV', joining: 'joining the TV\u2019s network',
        connecting: 'connecting to the TV'},
    directing: (name, step) => `Directly${name ? ` to ${name}` : ''}: ${step}…`,
    report: 'Report a problem…',
    reporting: 'Collecting the report…',
    reportWhat: 'What happened?',
    reportHint: 'e.g. the picture stutters with HEVC',
    reportHide: 'Hide addresses and names',
    reportMake: 'Make the report',
    reportReady: 'The report is ready',
    reportBody: file => `${file} is in Downloads. Nothing was sent: drag it into a chat or attach it to an issue.`,
    reportShow: 'Show the file',
    reportCopy: 'Copy description',
    reportIssue: 'Open an issue',
    reportSend: 'Send to the developers',
    reportSending: 'Sending the report to the developers…',
    reportSent: n => `Sent. Report number ${n} — tell it to us if you write in the chat.`,
    reportSentNoFile: n => `Sent. Report number ${n} — tell it to us if you write in the chat. The server did not take the file itself: send it if we ask.`,
    reportNotSent: e => `Not sent: ${reason(e)}. The file is still in Downloads: show it, drag it into a chat or attach it to an issue.`,
    reportCopyNumber: 'Copy the number',
    close: 'Close',
};

/** A D-Bus error's own message, without the "GDBus.Error:...: " prefix. */
function reason(e) {
    return String(e?.message ?? e).replace(/^GDBus\.Error:[^ ]+ /, '');
}

/** What the discovery ladder is trying now (FindProgress JSON), for the menu; null when it is not running. */
function findLine(json, t) {
    let p;
    try {
        p = JSON.parse(json);
    } catch (e) {
        return null;
    }
    if (!p?.running)
        return null;
    const now = (p.rungs ?? []).filter(r => r.state === 'running').map(r => t.rungs[r.rung] ?? r.rung);
    return now.length > 0 ? t.trying(now.join(', ')) : null;
}

/** What a direct connection is doing (DirectChanged JSON), for the menu; null when it is not under way. */
function directLine(json, t) {
    let s;
    try {
        s = JSON.parse(json);
    } catch (e) {
        return null;
    }
    const step = t.directSteps[s?.step];
    return step ? t.directing(s.name ?? '', step) : null;
}

/** A refusal in plain words with what to do (plain.js); nothing when the person cancelled it themselves. */
function tell(title, e) {
    const text = plainProblem(reason(e), RU);
    if (text !== null)
        Main.notify(title, text);
}

// luchd's refusal while a receiver waits for the code on its screen (Connect, Mirror, Cast...).
function asksForPairing(text) {
    return /asks for pairing/.test(text);
}

// The code from the TV, typed in a small dialog; null when cancelled.
function askCode(name) {
    const dialog = new ModalDialog.ModalDialog({destroyOnClose: true});
    const box = new St.BoxLayout({vertical: true, style: 'spacing: 12px; min-width: 320px;'});
    box.add_child(new St.Label({text: T.codeTitle(name), style: 'font-weight: bold; font-size: 1.2em;'}));
    box.add_child(new St.Label({text: T.codeHint}));
    const entry = new St.Entry({can_focus: true, hint_text: '123 456', style: 'font-size: 1.6em;'});
    box.add_child(entry);
    dialog.contentLayout.add_child(box);
    return new Promise(resolve => {
        let answered = false;
        const done = value => {
            if (answered)
                return;
            answered = true;
            dialog.close();
            resolve(value);
        };
        entry.clutter_text.connect('activate', () => done(entry.get_text()));
        dialog.connect('closed', () => done(null));
        dialog.setButtons([
            {label: T.cancel, action: () => done(null), key: Clutter.KEY_Escape},
            {label: T.pair, action: () => done(entry.get_text()), default: true},
        ]);
        dialog.open();
        global.stage.set_key_focus(entry);
    });
}

/** A question with Cancel and `yes`; true when the person chose `yes`. */
function confirm(title, body, yes) {
    const dialog = new ModalDialog.ModalDialog({destroyOnClose: true});
    const box = new St.BoxLayout({vertical: true, style: 'spacing: 12px; max-width: 420px;'});
    box.add_child(new St.Label({text: title, style: 'font-weight: bold; font-size: 1.2em;'}));
    const text = new St.Label({text: body});
    text.clutter_text.line_wrap = true;
    box.add_child(text);
    dialog.contentLayout.add_child(box);
    return new Promise(resolve => {
        let answered = false;
        const done = value => {
            if (answered)
                return;
            answered = true;
            dialog.close();
            resolve(value);
        };
        dialog.connect('closed', () => done(false));
        dialog.setButtons([
            {label: T.cancel, action: () => done(false), key: Clutter.KEY_Escape},
            {label: yes, action: () => done(true), default: true},
        ]);
        dialog.open();
    });
}

/** The report's short form: what happened, and whether to hide addresses and names (on by default). */
function reportForm() {
    const dialog = new ModalDialog.ModalDialog({destroyOnClose: true});
    const box = new St.BoxLayout({vertical: true, style: 'spacing: 12px; min-width: 420px;'});
    box.add_child(new St.Label({text: T.reportWhat, style: 'font-weight: bold; font-size: 1.2em;'}));
    const entry = new St.Entry({can_focus: true, hint_text: T.reportHint, style: 'min-height: 5em;'});
    entry.clutter_text.single_line_mode = false;
    entry.clutter_text.activatable = false;
    entry.clutter_text.line_wrap = true;
    box.add_child(entry);
    const hide = new CheckBox(T.reportHide);
    hide.checked = true;
    box.add_child(hide);
    dialog.contentLayout.add_child(box);
    return new Promise(resolve => {
        let answered = false;
        const done = value => {
            if (answered)
                return;
            answered = true;
            dialog.close();
            resolve(value);
        };
        dialog.connect('closed', () => done(null));
        dialog.setButtons([
            {label: T.cancel, action: () => done(null), key: Clutter.KEY_Escape},
            {label: T.reportMake, action: () => done({text: entry.get_text(), hide: hide.checked}), default: true},
        ]);
        dialog.open();
        global.stage.set_key_focus(entry);
    });
}

/**
 * The report written: where it is, who gave no log, and what to do with it; the buttons leave the dialog open.
 * Returns {say(text, actions)}: a line under it and new buttons (sending, sent, not sent).
 */
function reportDone(r, actions) {
    const dialog = new ModalDialog.ModalDialog({destroyOnClose: true});
    const box = new St.BoxLayout({vertical: true, style: 'spacing: 12px; max-width: 480px;'});
    box.add_child(new St.Label({text: T.reportReady, style: 'font-weight: bold; font-size: 1.2em;'}));
    const file = GLib.path_get_basename(r.path ?? '');
    const text = new St.Label({text: [T.reportBody(file), ...(r.notes ?? [])].join('\n')});
    text.clutter_text.line_wrap = true;
    box.add_child(text);
    const status = new St.Label({text: '', visible: false, style: 'font-weight: bold;'});
    status.clutter_text.line_wrap = true;
    box.add_child(status);
    dialog.contentLayout.add_child(box);
    const buttons = list => dialog.setButtons([
        ...list.map(([label, fn]) => ({label, action: fn})),
        {label: T.close, action: () => dialog.close(), key: Clutter.KEY_Escape, default: true},
    ]);
    buttons(actions);
    dialog.open();
    return {
        say(line, list) {
            status.text = line;
            status.visible = true;
            buttons(list);
        },
    };
}

/** The file selected in the file manager (FileManager1, as Files and most others serve it), else its folder. */
function showFile(path) {
    const file = Gio.File.new_for_path(path);
    Gio.DBus.session.call('org.freedesktop.FileManager1', '/org/freedesktop/FileManager1',
        'org.freedesktop.FileManager1', 'ShowItems', new GLib.Variant('(ass)', [[file.get_uri()], '']),
        null, Gio.DBusCallFlags.NONE, -1, null, (bus, res) => {
            try {
                bus.call_finish(res);
            } catch (e) {
                Gio.AppInfo.launch_default_for_uri(file.get_parent().get_uri(), null);
            }
        });
}

/** A TV without the Luch app (luchd's "cast:" and "dlna:" ids): it plays files, it cannot mirror or sleep. */
function isTv(id) {
    return /^(cast|dlna):/.test(id);
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

/** Starts luchd's user service (install.sh sets it up); it shows in the menu by itself once it is on the bus. */
function startService() {
    try {
        Gio.Subprocess.new(['systemctl', '--user', 'start', 'luchd.service'], Gio.SubprocessFlags.STDERR_PIPE)
            .communicate_utf8_async(null, null, (p, res) => {
                try {
                    const [, , err] = p.communicate_utf8_finish(res);
                    if (!p.get_successful())
                        Main.notify(T.noDaemon, err?.trim() ?? '');
                } catch (e) {
                    Main.notify(T.noDaemon, reason(e));
                }
            });
    } catch (e) {
        Main.notify(T.noDaemon, reason(e));
    }
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
        this._afterPairing = new Map(); // id -> the action that found the receiver asking for pairing
        this._typing = new Set(); // ids whose code was just typed: their failure is told by Pair itself
        this._waking = new Set();
        this._lastId = null;
        this._statsTimer = 0;
        this._finding = null; // what the discovery ladder is trying, from FindProgress
        this._direct = {offers: [], leaves: null}; // TVs that offer a direct connection (DirectOffers)
        this._directState = {step: 'idle'}; // where a direct connection stands (DirectChanged)
        this._group = null; // the group playing (Stats' group part), null when none
        this._delayItem = null; // its "how far behind" line, refreshed every second

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
            this._pairId = proxy.connectSignal('PairChanged', (_p, _s, [id, state]) => this._onPair(id, state));
            this._findId = proxy.connectSignal('FindProgress', (_p, _sender, [status]) => {
                this._finding = findLine(status, T);
                this._render(null);
            });
            this._directId = proxy.connectSignal('DirectChanged', (_p, _sender, [status]) => this._onDirect(status));
            proxy.DirectStatusAsync().then(([status]) => {
                this._directState = JSON.parse(status);
            }).catch(() => {}); // an older luchd
            this._quiet = new QuietAsker(proxy, askQuestion, T);
            this._reporter = new Reporter(proxy, {
                form: reportForm,
                busy: on => {
                    this._reporting = on;
                    this._render(null);
                },
                done: reportDone,
                failed: e => tell(T.failed(T.title), e),
            }, {
                showFile,
                copy: text => St.Clipboard.get_default().set_text(St.ClipboardType.CLIPBOARD, text),
                open: url => Gio.AppInfo.launch_default_for_uri(url, null),
            }, T);
            this._quietId = proxy.connectSignal('QuietAsk', () => this._quiet.onAsk());
            this._ownerId = proxy.connect('notify::g-name-owner', () => this._discover());
            this._propId = proxy.connect('g-properties-changed', () => this._refresh());
            this._discover();
        }, null, Gio.DBusProxyFlags.DO_NOT_AUTO_START);
    }

    // A receiver asking for pairing: "allow" waits for the TV, "code" asks for the code here.
    async _onPair(id, json) {
        const state = JSON.parse(json);
        const name = state.name ?? id;
        if (state.step === 'allow') {
            Main.notify(T.allowOnTv(name), '');
        } else if (state.step === 'code') {
            const code = await askCode(name);
            if (code === null)
                return;
            this._typing.add(id);
            try {
                await this._proxy.PairAsync(id, code);
                Main.notify(T.paired(name), '');
                const again = this._afterPairing.get(id);
                this._afterPairing.delete(id);
                await again?.();
            } catch (e) {
                tell(T.failed(name), e);
            } finally {
                this._typing.delete(id);
            }
        } else if (state.step === 'failed') {
            this._afterPairing.delete(id);
            if (!this._typing.has(id))
                tell(T.failed(name), state.reason ?? '');
        }
    }

    // A direct connection's steps: the menu follows them; its end is told.
    _onDirect(json) {
        const s = JSON.parse(json);
        this._directState = s;
        const name = s.name ?? T.title;
        if (s.step === 'on')
            Main.notify(T.directOn(name), '');
        else if (s.step === 'failed')
            tell(T.failed(name), s.reason ?? '');
        else if (s.step === 'back')
            Main.notify(T.title, s.note ?? '');
        this._render(null);
    }

    // Asked first: it takes this computer off its Wi-Fi while showing.
    async _connectDirectly(key, name) {
        let leaves = this._direct.leaves;
        try {
            leaves = JSON.parse((await this._proxy.DirectOffersAsync())[0]).leaves;
        } catch (e) {}
        if (!await confirm(T.directTitle(name), T.directBody(leaves), T.directGo))
            return;
        try {
            await this._proxy.DirectAsync(key);
        } catch (e) {
            tell(T.failed(name ?? T.title), e);
        }
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
            const [json] = await this._proxy.DirectOffersAsync();
            this._direct = JSON.parse(json);
        } catch (e) {
            this._direct = {offers: [], leaves: null}; // an older luchd
        }
        this._group = null;
        const playing = this._mirroring[0];
        if (playing) {
            try {
                this._group = Group.groupOf((await this._proxy.StatsAsync(playing[0]))[0]);
            } catch (e) {}
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
        this._delayItem = null;
        const group = this._group;
        const inGroup = id => group?.members.some(m => m.id === id) ?? false;
        // What is happening now comes first, with the way to stop it: no submenu to open for that. A group stops
        // as one.
        if (group) {
            const item = new PopupMenu.PopupImageMenuItem(G.stopAll, 'media-playback-stop-symbolic');
            const [one] = group.members;
            item.connect('activate', () => this._proxy.StopMirrorAsync(one.id).catch(e => tell(T.failed(one.name), e)));
            this._section.addMenuItem(item);
        }
        for (const [id, name] of mirroring.filter(r => !inGroup(r[0]))) {
            const item = new PopupMenu.PopupImageMenuItem(T.stopOn(name), 'media-playback-stop-symbolic');
            item.connect('activate', () => this._proxy.StopMirrorAsync(id).catch(e => tell(T.failed(name), e)));
            this._section.addMenuItem(item);
        }
        const empty = this._rows.length === 0 && this._asleep.length === 0;
        if (note || this._finding || empty) {
            const item = new PopupMenu.PopupMenuItem(note ?? this._finding ?? T.searching, {reactive: false});
            this._section.addMenuItem(item);
        }
        if (note === T.noDaemon)
            this._section.addAction(T.startDaemon, startService);
        // The first time nothing is found: where the TV comes from, before any way to look harder.
        if (!note && empty)
            this._section.addMenuItem(new PopupMenu.PopupMenuItem(T.firstRun, {reactive: false}));
        // Nothing on the list and nothing being tried: the rest of the ladder, which needs the person (the TV's
        // sound through the microphone, its QR code through the camera).
        if (!note && !this._finding && this._rows.length === 0 && this._proxy?.g_name_owner) {
            this._section.addAction(T.findHarder, () => this._proxy.FindAsync('all')
                .catch(e => logError(e, 'Luch: find')));
        }
        // The last way: straight over the TV's own Wi-Fi network, for a TV heard over Bluetooth that no network
        // reaches. Never without asking.
        const directing = directLine(JSON.stringify(this._directState), T);
        if (directing) {
            this._section.addMenuItem(new PopupMenu.PopupMenuItem(directing, {reactive: false}));
        } else if (this._directState.step === 'on') {
            this._section.addAction(T.directOff(this._directState.previous), () => this._proxy.DirectOffAsync()
                .catch(e => tell(T.failed(T.title), e)));
        } else if (this._proxy?.g_name_owner) {
            const listed = key => this._rows.some(r => r[0].replace(/-/g, '').startsWith(key));
            const offers = (this._direct.offers ?? []).filter(o => !listed(o.key));
            for (const o of offers)
                this._section.addAction(T.directTo(o.name), () => this._connectDirectly(o.key, o.name));
            if (offers.length === 0 && !note && !this._finding && this._rows.length === 0)
                this._section.addAction(T.direct, () => this._connectDirectly('', null));
        }
        for (const [id, name, model, , , , connected, isMirroring] of this._rows) {
            const sub = new PopupMenu.PopupSubMenuMenuItem(name, true);
            if (isMirroring)
                sub.icon.icon_name = 'media-playback-start-symbolic';
            else
                sub.icon.gicon = ICON;
            sub.label.text = [name, model, isMirroring ? T.mirroringNow : ''].filter(Boolean).join(' · ');
            const add = (label, fn) => sub.menu.addAction(label, this._guarded(id, name, fn));
            if (isMirroring && inGroup(id)) {
                add(G.leave, () => this._proxy.RemoveFromGroupAsync(id));
            } else if (isMirroring) {
                add(T.stop, () => this._proxy.StopMirrorAsync(id));
            } else if (group && Group.candidates([[id]]).length > 0) {
                add(G.addTo, () => this._proxy.AddToGroupAsync(group.members[0].id, id, ''));
            } else if (!isTv(id)) {
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
            // A presentation needs the receiver's document view, which TVs without Luch do not have.
            if (!isTv(id)) {
                add(T.present, async () => {
                    const [file] = await chooseFiles(T.choosePresentation, false);
                    if (!file)
                        return;
                    const [url] = await this._proxy.PresentAsync(id, file);
                    Gio.AppInfo.launch_default_for_uri(url, null);
                });
            }
            if (connected)
                add(T.stopCast, () => this._proxy.StopCastAsync(id));
            if (isTv(id)) {
                this._section.addMenuItem(sub);
                continue;
            }
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

        this._renderGroup(group);

        // The settings and the rarely used, out of the way: do not disturb, watching the TV, the web receiver's address.
        const mode = this._proxy?.Quiet;
        const watch = this._proxy?.WatchDevice; // null from a luchd without it
        const url = this._proxy?.WebUrl ?? '';
        if (mode || typeof watch === 'boolean' || url) {
            const more = new PopupMenu.PopupSubMenuMenuItem(T.more, true);
            more.icon.icon_name = 'view-more-symbolic';
            // Do not disturb while presenting or mirroring: the three choices in sight, the one in use marked.
            if (mode) {
                more.menu.addMenuItem(new PopupMenu.PopupMenuItem(T.quiet, {reactive: false}));
                for (const each of ['ask', 'on', 'off']) {
                    const item = more.menu.addAction(T.quietModes[each], () => {
                        this._proxy.Quiet = each;
                    });
                    item.setOrnament(each === mode ? PopupMenu.Ornament.DOT : PopupMenu.Ornament.NONE);
                }
            }
            // Off by default: a hot TV then shows no badge over the picture (PROTOCOL.md 5); from the next start on.
            if (typeof watch === 'boolean') {
                if (mode)
                    more.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
                const item = new PopupMenu.PopupSwitchMenuItem(T.watchDevice, watch);
                item.connect('toggled', (_item, on) => {
                    this._proxy.WatchDevice = on;
                });
                more.menu.addMenuItem(item);
            }
            // Built-in web receiver: the URL for the TV; opening it on the PC is a quick test.
            if (url) {
                if (mode || typeof watch === 'boolean')
                    more.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
                more.menu.addMenuItem(new PopupMenu.PopupMenuItem(T.web, {reactive: false}));
                const item = new PopupMenu.PopupMenuItem(url, {reactive: false});
                item.label.add_style_class_name('monospace');
                more.menu.addMenuItem(item);
                more.menu.addAction(T.webOpen, () => {
                    try {
                        Gio.AppInfo.launch_default_for_uri(url, null);
                    } catch (e) {
                        logError(e, 'Luch');
                    }
                });
            }
            this._section.addMenuItem(more);
        }

        // Everything a tester hands over, as one file; nothing is sent by itself.
        if (this._reporter) {
            const item = new PopupMenu.PopupImageMenuItem(this._reporting ? T.reporting : T.report,
                'dialog-warning-symbolic');
            item.setSensitive(!this._reporting);
            item.connect('activate', () => this._reporter.run());
            this._section.addMenuItem(item);
        }

        if (mirroring.length > 0) {
            this.subtitle = mirroring[0][1];
            this._startStats();
        } else {
            this._stopStats();
            this.subtitle = note ?? (this._rows.length === 1 ? this._rows[0][1] : T.receivers(this._rows.length));
        }
    }

    // Several TVs at once: while a group plays, who is in it with Remove, how far behind it is, and Add for the
    // others; else the one entry to start one.
    _renderGroup(group) {
        if (!group) {
            const can = Group.candidates(this._rows);
            if (can.length >= 2)
                this._section.addAction(G.entry, () => this._startGroup(can));
            return;
        }
        const sub = new PopupMenu.PopupSubMenuMenuItem(Group.title(group, G), true);
        sub.icon.icon_name = 'media-playback-start-symbolic';
        this._delayItem = new PopupMenu.PopupMenuItem(Group.delayLine(group, G), {reactive: false});
        sub.menu.addMenuItem(this._delayItem);
        const names = new Map(this._rows.map(r => [r[0], r[1]]));
        for (const [id, label] of Group.removals(group, G))
            sub.menu.addAction(label, this._guarded(id, names.get(id) ?? id, () => this._proxy.RemoveFromGroupAsync(id)));
        const others = Group.addable(this._rows, group);
        if (others.length > 0)
            sub.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());
        for (const [id, name] of others) {
            sub.menu.addAction(G.add(name),
                this._guarded(id, name, () => this._proxy.AddToGroupAsync(group.members[0].id, id, '')));
        }
        this._section.addMenuItem(sub);
    }

    async _startGroup(receivers) {
        const p = await chooseGroup(receivers, G);
        if (!p)
            return;
        const [first] = receivers.filter(r => r[0] === p.ids[0]);
        this._guarded(p.ids[0], first?.[1] ?? T.title,
            () => this._proxy.MirrorGroupAsync(p.ids, p.roles, p.source, p.mode))();
    }

    async _wake(id) {
        this._waking.add(id);
        this._render(null);
        try {
            await this._proxy.WakeAsync(id);
        } catch (e) {
            logError(e, 'Luch: wake');
            tell(T.failed(this._asleep.find(r => r[0] === id)?.[1] ?? T.title), e);
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

    /**
     * `fn` for a menu item or the tile: a refusal from luchd (the TV off, the accessibility switch off...) is told
     * in plain words, not only logged; a TV that shows a code gets its dialog with PairChanged, and `fn` runs again
     * once paired.
     */
    _guarded(id, name, fn) {
        return () => fn().catch(e => {
            if (asksForPairing(reason(e))) {
                this._afterPairing.set(id, fn);
                return;
            }
            logError(e, 'Luch');
            tell(T.failed(name), e);
        });
    }

    _onClicked() {
        const mirroring = this._mirroring;
        if (mirroring.length > 0) {
            for (const [id, name] of mirroring)
                this._proxy.StopMirrorAsync(id).catch(e => tell(T.failed(name), e));
            return;
        }
        // One tap = the lowest-latency mode to the last (or only/first) receiver.
        const target = this._rows.find(r => r[0] === this._lastId) ?? this._rows.find(r => !isTv(r[0]));
        if (target)
            this._guarded(target[0], target[1], () => this._mirror(target[0], 'fast'))();
        else
            this.menu.open();
    }

    _startStats() {
        if (this._statsTimer)
            return;
        this._statsTimer = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, 1, () => {
            // Whoever plays now: in a group, the first one may have left it meanwhile.
            const id = this._mirroring[0]?.[0];
            if (!id || !this._proxy)
                return GLib.SOURCE_CONTINUE;
            this._proxy.StatsAsync(id).then(([json]) => {
                const s = JSON.parse(json);
                const name = this._mirroring[0]?.[1] ?? '';
                if (s.est_ms)
                    this.subtitle = `${name} · ≈${Math.round(s.est_ms)} ms`;
                // A group's members come and go: the menu follows; its delay line follows every second.
                const g = Group.groupOf(s);
                if (!Group.sameMembers(g, this._group)) {
                    this._group = g;
                    this._render(null);
                } else if (g) {
                    this._group = g;
                    this._delayItem?.label.set_text(Group.delayLine(g, G));
                }
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
        if (this._pairId)
            this._proxy.disconnectSignal(this._pairId);
        if (this._findId)
            this._proxy.disconnectSignal(this._findId);
        if (this._directId)
            this._proxy.disconnectSignal(this._directId);
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
