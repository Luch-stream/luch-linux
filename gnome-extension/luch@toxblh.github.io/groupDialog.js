// The "Show on several TVs" dialog: the three ready choices, the TVs ticked, the picture's TV for "Screen here,
// sound there", and the timing folded under "More options". Its state and words are group.js's (tested there).
// SPDX-License-Identifier: GPL-3.0-or-later
import Clutter from 'gi://Clutter';
import St from 'gi://St';

import {CheckBox} from 'resource:///org/gnome/shell/ui/checkBox.js';
import * as ModalDialog from 'resource:///org/gnome/shell/ui/modalDialog.js';
import {Choice, PRESET_ORDER} from './group.js';

const CHOSEN = 'border: 2px solid -st-accent-color; border-radius: 8px; padding: 6px 10px;';
const PLAIN = 'border: 2px solid transparent; border-radius: 8px; padding: 6px 10px;';

/** A button that is one of a few, marked when chosen. */
function option(text, sub) {
    const box = new St.BoxLayout({vertical: true});
    box.add_child(new St.Label({text, style: 'font-weight: bold;'}));
    if (sub) {
        const l = new St.Label({text: sub, style: 'font-size: 0.9em; opacity: 0.75;'});
        l.clutter_text.line_wrap = true;
        box.add_child(l);
    }
    return new St.Button({child: box, can_focus: true, x_expand: true, style: PLAIN});
}

/** Asks what to show where; the MirrorGroup call ({ids, roles, source, mode}), or null when cancelled. */
export function chooseGroup(receivers, t) {
    const choice = new Choice(receivers, t);
    const dialog = new ModalDialog.ModalDialog({destroyOnClose: true});
    const box = new St.BoxLayout({vertical: true, style: 'spacing: 10px; min-width: 440px; max-width: 520px;'});
    box.add_child(new St.Label({text: t.title, style: 'font-weight: bold; font-size: 1.2em;'}));

    box.add_child(new St.Label({text: t.what, style: 'opacity: 0.75;'}));
    const presets = PRESET_ORDER.map(p => {
        const b = option(...t.presets[p]);
        b.connect('clicked', () => {
            choice.setPreset(p);
            draw();
        });
        box.add_child(b);
        return [p, b];
    });

    box.add_child(new St.Label({text: t.which, style: 'opacity: 0.75; margin-top: 6px;'}));
    const rows = receivers.map(([id, name]) => {
        const row = new St.BoxLayout({style: 'spacing: 12px;'});
        const check = new CheckBox(name);
        check.x_expand = true;
        check.connect('clicked', () => {
            choice.tick(id, check.checked);
            draw();
        });
        // "Screen here, sound there": which one shows the picture; the others play its sound.
        const role = new St.Button({label: '', can_focus: true, style: PLAIN});
        role.connect('clicked', () => {
            choice.setPicture(id);
            draw();
        });
        row.add_child(check);
        row.add_child(role);
        box.add_child(row);
        return {id, check, role};
    });

    const more = new St.Button({label: `${t.more} ▸`, can_focus: true, x_align: Clutter.ActorAlign.START,
        style: 'margin-top: 6px;'});
    const timingBox = new St.BoxLayout({vertical: true, style: 'spacing: 4px;', visible: false});
    timingBox.add_child(new St.Label({text: t.timing, style: 'opacity: 0.75;'}));
    const timings = ['watch', 'interactive'].map(m => {
        const b = option(t.timings[m]);
        b.connect('clicked', () => {
            choice.setTiming(m);
            draw();
        });
        timingBox.add_child(b);
        return [m, b];
    });
    more.connect('clicked', () => {
        timingBox.visible = !timingBox.visible;
        more.label = `${t.more} ${timingBox.visible ? '▾' : '▸'}`;
    });
    box.add_child(more);
    box.add_child(timingBox);

    const error = new St.Label({text: '', style: 'color: #e66;'});
    box.add_child(error);
    dialog.contentLayout.add_child(box);

    let show = null;
    function draw() {
        const v = choice.view();
        for (const [p, b] of presets)
            b.style = p === choice.preset ? CHOSEN : PLAIN;
        for (const r of rows) {
            const row = v.rows.find(x => x.id === r.id);
            r.check.checked = row.ticked;
            r.role.visible = row.picture !== null && row.ticked;
            r.role.label = row.picture ? t.picture : t.sound;
            r.role.style = row.picture ? CHOSEN : PLAIN;
        }
        more.visible = v.timing !== null;
        timingBox.visible &&= v.timing !== null;
        for (const [m, b] of timings)
            b.style = m === v.timing ? CHOSEN : PLAIN;
        error.text = v.error ?? '';
        if (show)
            show.reactive = v.ok;
    }

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
            {label: t.cancel, action: () => done(null), key: Clutter.KEY_Escape},
            {label: t.start, action: () => choice.result() && done(choice.result()), default: true},
        ]);
        show = dialog.buttonLayout.get_last_child();
        draw();
        dialog.open();
    });
}
