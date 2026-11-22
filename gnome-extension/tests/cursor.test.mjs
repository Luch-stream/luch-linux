import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';

const source = readFileSync(new URL('../luch@toxblh.github.io/cursor.js', import.meta.url), 'utf8')
    .replace(/^import .*;\n/gm, '').replace('export default class VirtualCursor', 'class VirtualCursor');
const flush = () => new Promise(resolve => setImmediate(resolve));
class Variant {
    constructor(type, value) { this.value = value; }
    deepUnpack() { return this.value; }
}
function fixture(currentFlag = new Variant('b', true)) {
    let pointer = [2100, 100];
    const calls = [];
    const texture = {get_width: () => 32, get_height: () => 48};
    const tracker = {connect: () => 1, disconnect() {}, get_sprite: () => texture,
        get_pointer_visible: () => true, get_hot: () => [4, 6]};
    const group = {add_child() {}, set_child_above_sibling() {}};
    const Main = {uiGroup: group, sessionMode: {isLocked: false}};
    class Actor {
        constructor(props) { Object.assign(this, props); }
        hide() { this.visible = false; }
        show() { this.visible = true; }
        set_size(w, h) { this.size = [w, h]; }
        set_position(x, y) { this.position = [x, y]; }
        get_parent() { return group; }
        destroy() { this.destroyed = true; }
    }
    const spec = ['Meta-0', 'MetaVendor', 'Virtual remote monitor', '0x1'];
    const answers = {
        Receivers: [[['id', 'TV', '', '', '', 47800, true, true]]],
        Stats: [JSON.stringify({sender: {virtual_monitor: 'Meta-0'}})],
        GetCurrentState: [1, [[spec, [['mode', 3840, 2160, 60, 1, [1, 2], {'is-current': currentFlag}]], {}]],
            [[1920, 0, 2, 0, false, [spec], {}]], {}],
    };
    const Gio = {DBusCallFlags: {NO_AUTO_START: 1}, DBus: {session: {
        call(name, path, iface, method, args, out, flags, timeout, cancel, callback) {
            calls.push(method);
            queueMicrotask(() => callback({call_finish: () => new Variant('', answers[method])}, {}));
        },
    }}};
    const context = vm.createContext({Gio, Main, GLib: {Variant, timeout_add_seconds: () => 1, source_remove() {}},
        GObject: {registerClass: (meta, type) => type, Object: class {invalidate() {}}},
        Clutter: {Content: {}, Actor}, PointerWatcher: {getPointerWatcher: () => ({addWatch: () => ({remove() {}})})},
        global: {get_pointer: () => pointer, backend: {get_cursor_tracker: () => tracker}}, console});
    vm.runInContext(source + '\nglobalThis.cursor = new VirtualCursor();', context);
    return {cursor: context.cursor, Main, calls, answers, move: (x, y) => {pointer = [x, y]; context.cursor._updatePointer();}};
}

test('draws real cursor with hotspot on our scaled virtual monitor only', async () => {
    const f = fixture(); await flush();
    assert.equal(f.cursor._actor.visible, true);
    assert.deepEqual(Array.from(f.cursor._actor.position), [2098, 97]);
    assert.deepEqual(Array.from(f.cursor._actor.size), [16, 24]);
    f.move(100, 100);
    assert.equal(f.cursor._actor.visible, false);
    f.move(2500, 500);
    assert.equal(f.cursor._actor.visible, true);
    f.Main.sessionMode.isLocked = true;
    f.move(2501, 500);
    assert.equal(f.cursor._actor.visible, false);
    f.cursor.destroy();
});

test('accepts unpacked mode flags and removes cursor when stream stops', async () => {
    const f = fixture(true); await flush();
    assert.equal(f.cursor._actor.visible, true);
    f.answers.Stats = ['{}'];
    await f.cursor._refresh();
    assert.equal(f.cursor._actor.visible, false);
    assert.equal(f.cursor._watch, null);
    f.cursor.destroy();
});

test('late D-Bus answers after disabling do not recreate the overlay', async () => {
    const f = fixture(); f.cursor.destroy(); await flush();
    assert.equal(f.cursor._actor, null);
    assert.equal(f.cursor._alive, false);
});
