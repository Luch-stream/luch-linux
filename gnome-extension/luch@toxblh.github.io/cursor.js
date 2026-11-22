// SPDX-License-Identifier: GPL-3.0-or-later
import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PointerWatcher from 'resource:///org/gnome/shell/ui/pointerWatcher.js';

// Paint the actual cursor texture on the GPU, like GNOME's magnifier. No pixel readback.
const CursorContent = GObject.registerClass({Implements: [Clutter.Content]},
class CursorContent extends GObject.Object {
    vfunc_get_preferred_size() {
        return this.texture ? [true, this.texture.get_width(), this.texture.get_height()] : [false, 0, 0];
    }
    vfunc_paint_content(actor, node) {
        if (!this.texture)
            return;
        const [min, mag] = actor.get_content_scaling_filters();
        const image = new Clutter.TextureNode(this.texture, null, min, mag);
        image.add_rectangle(actor.get_content_box());
        node.add_child(image);
    }
});

function call(name, path, iface, method, args = null) {
    return new Promise((resolve, reject) => {
        Gio.DBus.session.call(name, path, iface, method, args, null,
            Gio.DBusCallFlags.NO_AUTO_START, 3000, null, (conn, result) => {
                try {
                    resolve(conn.call_finish(result).deepUnpack());
                } catch (e) {
                    reject(e);
                }
            });
    });
}
const unpack = value => value instanceof GLib.Variant ? value.deepUnpack() : value;
const luch = (method, args) => call('dev.luch.Luch1', '/dev/luch/Luch1', 'dev.luch.Luch1', method, args);

export default class VirtualCursor {
    constructor() {
        this._alive = true;
        this._busy = false;
        this._areas = [];
        this._content = new CursorContent();
        this._actor = new Clutter.Actor({content: this._content, reactive: false, visible: false});
        Main.uiGroup.add_child(this._actor);
        this._tracker = global.backend.get_cursor_tracker();
        this._spriteSignal = this._tracker.connect('cursor-changed', () => this._updatePointer());
        this._timer = GLib.timeout_add_seconds(GLib.PRIORITY_DEFAULT, 1, () => {
            this._refresh();
            return GLib.SOURCE_CONTINUE;
        });
        this._refresh();
    }

    async _refresh() {
        if (this._busy)
            return;
        this._busy = true;
        let areas = [];
        try {
            const [rows] = await luch('Receivers');
            const stats = await Promise.all(rows.filter(r => r[7]).map(async r => {
                const [text] = await luch('Stats', new GLib.Variant('(s)', [r[0]]));
                return JSON.parse(text);
            }));
            const connectors = new Set(stats.map(s => s?.sender?.virtual_monitor).filter(Boolean));
            if (connectors.size > 0) {
                const [, monitors, logical] = await call('org.gnome.Mutter.DisplayConfig',
                    '/org/gnome/Mutter/DisplayConfig', 'org.gnome.Mutter.DisplayConfig', 'GetCurrentState');
                for (const [x, y, scale, transform, , specs] of logical) {
                    const spec = specs.find(s => connectors.has(s[0]));
                    if (!spec)
                        continue;
                    const monitor = monitors.find(m => m[0][0] === spec[0]);
                    const mode = monitor?.[1].find(m => unpack(m[6]['is-current']));
                    if (!mode)
                        continue;
                    const width = (transform % 2 ? mode[2] : mode[1]) / scale;
                    const height = (transform % 2 ? mode[1] : mode[2]) / scale;
                    areas.push({x, y, width, height, scale});
                }
            }
        } catch {
            // The daemon or virtual screen may have stopped between calls.
        } finally {
            this._busy = false;
        }
        if (!this._alive)
            return;
        this._areas = areas;
        if (areas.length > 0 && !this._watch)
            this._watch = PointerWatcher.getPointerWatcher().addWatch(16, () => this._updatePointer());
        else if (areas.length === 0 && this._watch) {
            this._watch.remove();
            this._watch = null;
        }
        this._updatePointer();
    }

    _updatePointer() {
        if (!this._alive)
            return;
        const [x, y] = global.get_pointer();
        const area = this._areas.find(a => x >= a.x && x < a.x + a.width && y >= a.y && y < a.y + a.height);
        const texture = area && !Main.sessionMode.isLocked && this._tracker.get_pointer_visible()
            ? this._tracker.get_sprite() : null;
        if (!texture) {
            this._actor.hide();
            return;
        }
        this._content.texture = texture;
        this._content.invalidate();
        const [hotX, hotY] = this._tracker.get_hot();
        this._actor.set_size(texture.get_width() / area.scale, texture.get_height() / area.scale);
        this._actor.set_position(x - hotX / area.scale, y - hotY / area.scale);
        this._actor.get_parent().set_child_above_sibling(this._actor, null);
        this._actor.show();
    }

    destroy() {
        this._alive = false;
        GLib.source_remove(this._timer);
        this._watch?.remove();
        this._watch = null;
        this._tracker.disconnect(this._spriteSignal);
        this._actor.destroy();
        this._actor = null;
        this._content = null;
        this._areas = [];
    }
}
