// Showing on several TVs at once (luchd's groups, PROTOCOL.md 4k), in words for people who know nothing about it:
// three ready choices instead of roles and modes, the TVs to tick, and while it plays who is in it, how far behind
// the screen it is, and Remove / Add. Only the logic and the words are here, so that they are tested; the dialog is
// groupDialog.js, the menu items extension.js.
// SPDX-License-Identifier: GPL-3.0-or-later

/**
 * The ready choices. `screen`: this screen and its sound on every TV, all at one delay (rooms, a wall of TVs).
 * `music`: this computer's sound on every TV, as speakers in every room. `split`: the picture on one TV right away
 * and its sound on the others (a TV and a speaker), the sound waiting for the picture.
 */
export const PRESETS = {
    screen: {source: 'fast', mode: 'watch'},
    music: {source: 'audio', mode: 'watch'},
    split: {source: 'fast', mode: 'interactive'},
};

export const PRESET_ORDER = ['screen', 'music', 'split'];

const WORDS = {
    ru: {
        entry: 'Показать на нескольких ТВ…',
        title: 'Показать на нескольких ТВ',
        what: 'Что показать',
        presets: {
            screen: ['Этот экран на всех ТВ', 'Картинка и звук везде, все в такт'],
            music: ['Музыка во всех комнатах', 'Только звук этого компьютера, ТВ — как колонки'],
            split: ['Экран здесь, звук там', 'Картинка на одном ТВ сразу, звук на других — подстроен под неё'],
        },
        which: 'На каких ТВ',
        picture: 'картинка',
        sound: 'звук',
        more: 'Подробнее',
        timing: 'Если ТВ отстают друг от друга:',
        timings: {watch: 'Все в такт (для комнат и стены из ТВ)', interactive: 'Картинка без задержки (для работы и игр)'},
        start: 'Показать',
        cancel: 'Отмена',
        tickTwo: 'Отметьте два ТВ или больше',
        together: names => `Вместе: ${names.join(', ')}`,
        starting: 'Подстраиваю ТВ друг под друга…',
        inStep: s => `Все в такт, отстают от экрана на ${s} с`,
        pictureNow: 'Картинка и звук сразу',
        soundWaits: ms => `Картинка сразу, звук ждёт её ${ms} мс`,
        role: {video: 'картинка', audio: 'звук', both: 'картинка и звук'},
        remove: (name, role) => `Убрать ${name} (${role})`,
        add: name => `Добавить ${name}`,
        addTo: 'Добавить к показу на нескольких ТВ',
        leave: 'Убрать из показа на нескольких ТВ',
        stopAll: 'Остановить показ на всех ТВ',
    },
    en: {
        entry: 'Show on several TVs…',
        title: 'Show on several TVs',
        what: 'What to show',
        presets: {
            screen: ['This screen on all TVs', 'Picture and sound everywhere, all in step'],
            music: ['Music in every room', 'Only this computer’s sound, the TVs as speakers'],
            split: ['Screen here, sound there', 'The picture on one TV at once, its sound on the others, matched to it'],
        },
        which: 'On which TVs',
        picture: 'picture',
        sound: 'sound',
        more: 'More options',
        timing: 'When the TVs lag behind each other:',
        timings: {watch: 'All in step (rooms, a wall of TVs)', interactive: 'Picture without delay (work, games)'},
        start: 'Show',
        cancel: 'Cancel',
        tickTwo: 'Tick two TVs or more',
        together: names => `Together: ${names.join(', ')}`,
        starting: 'Matching the TVs to each other…',
        inStep: s => `All in step, ${s} s behind this screen`,
        pictureNow: 'Picture and sound right away',
        soundWaits: ms => `Picture right away, sound waits ${ms} ms for it`,
        role: {video: 'picture', audio: 'sound', both: 'picture and sound'},
        remove: (name, role) => `Remove ${name} (${role})`,
        add: name => `Add ${name}`,
        addTo: 'Add to the show on several TVs',
        leave: 'Remove from the show on several TVs',
        stopAll: 'Stop mirroring on all TVs',
    },
};

export function words(ru) {
    return WORDS[ru ? 'ru' : 'en'];
}

export const GROUP_WORDS = WORDS;

/** Receivers that can play in a group: Luch receivers (not TVs without Luch, not a browser), as [id, name]. */
export function candidates(rows) {
    return rows.filter(r => !/^(cast|dlna|web)[:-]/.test(r[0])).map(r => [r[0], r[1]]);
}

/**
 * What the dialog's choice asks luchd for (MirrorGroup's ids, roles, source, mode), or {error} to show instead.
 * `ticked`: the ids in the order of the list; `picture`: the one that shows the picture in `split`; `timing`:
 * the "More options" choice, null for the preset's own.
 */
export function plan(preset, ticked, picture, timing, t) {
    const p = PRESETS[preset];
    if (!p)
        throw new Error(`no preset ${preset}`);
    if (ticked.length < 2)
        return {error: t.tickTwo};
    let ids = [...ticked];
    let roles;
    if (preset === 'split') {
        // The picture's TV leads (luchd: the first one shows it); it plays no sound, the others are its speakers.
        const lead = ticked.includes(picture) ? picture : ticked[0];
        ids = [lead, ...ticked.filter(id => id !== lead)];
        roles = ids.map((_, i) => i === 0 ? 'video' : 'audio');
    } else {
        roles = ids.map(() => preset === 'music' ? 'audio' : 'both');
    }
    return {ids, roles, source: p.source, mode: preset === 'music' ? 'watch' : timing ?? p.mode};
}

/** The group part of a receiver's Stats, or null when it does not play in one. */
export function groupOf(stats) {
    let s = stats;
    if (typeof stats === 'string') {
        try {
            s = JSON.parse(stats);
        } catch (e) {
            return null;
        }
    }
    const g = s?.group;
    return g && Array.isArray(g.members) ? g : null;
}

/** How far behind the screen the group plays, in plain words. */
export function delayLine(g, t) {
    const dv = Number(g.d_video_ms ?? 0), da = Number(g.d_audio_ms ?? 0);
    if (g.mode === 'watch') {
        if (da <= 0)
            return t.starting;
        return t.inStep((Math.round(da / 10) / 100).toLocaleString(t === WORDS.ru ? 'ru' : 'en'));
    }
    if (da - dv < 5)
        return t.pictureNow;
    return t.soundWaits(Math.round((da - dv) / 10) * 10);
}

/** The group's item in the menu: its members' names. */
export function title(g, t) {
    return t.together(g.members.map(m => m.name));
}

/** Each member with its Remove item's words: [id, label]. */
export function removals(g, t) {
    return g.members.map(m => [m.id, t.remove(m.name, t.role[m.role] ?? m.role)]);
}

/** The receivers that could join: Luch receivers on the list and not in it already, as [id, name]. */
export function addable(rows, g) {
    const inside = new Set(g.members.map(m => m.id));
    return candidates(rows).filter(([id]) => !inside.has(id));
}

/** Whether two group parts list the same members in the same roles: the menu is built again only when not. */
export function sameMembers(a, b) {
    const key = g => g ? g.members.map(m => `${m.id}=${m.role}`).join(',') : '';
    return key(a) === key(b);
}

/**
 * The dialog's state: the ready choice, the TVs ticked (all, to begin with: the usual wish is "everywhere"), which
 * one shows the picture in `split`, and the timing under "More options". The dialog only draws `view()` and passes
 * clicks here.
 */
export class Choice {
    constructor(receivers, t) {
        this.t = t;
        this.receivers = receivers;
        this.preset = 'screen';
        this.ticked = new Set(receivers.map(r => r[0]));
        this.picture = receivers[0]?.[0] ?? null;
        this.timing = null;
    }

    setPreset(p) {
        this.preset = p;
    }

    tick(id, on) {
        if (on)
            this.ticked.add(id);
        else
            this.ticked.delete(id);
        // The picture's TV unticked: the first one still ticked takes it.
        if (!this.ticked.has(this.picture))
            this.picture = this._order().find(x => this.ticked.has(x)) ?? this.picture;
    }

    setPicture(id) {
        this.picture = id;
        this.ticked.add(id);
    }

    setTiming(mode) {
        this.timing = mode;
    }

    _order() {
        return this.receivers.map(r => r[0]);
    }

    _plan() {
        return plan(this.preset, this._order().filter(id => this.ticked.has(id)), this.picture, this.timing, this.t);
    }

    /** What the dialog shows: the rows, whether the picture's choice and the timing are there, and Show's state. */
    view() {
        const split = this.preset === 'split';
        const p = this._plan();
        return {
            rows: this.receivers.map(([id, name]) => ({id, name, ticked: this.ticked.has(id),
                picture: split ? id === this.picture : null})),
            timing: this.preset === 'music' ? null : this.timing ?? PRESETS[this.preset].mode,
            ok: !p.error,
            error: p.error ?? null,
        };
    }

    /** The MirrorGroup call, or null while it cannot start. */
    result() {
        const p = this._plan();
        return p.error ? null : p;
    }
}
