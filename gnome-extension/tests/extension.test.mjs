// The extension's D-Bus interface must name methods luchd really has, and luchd's refusals must read well.
import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';

const ext = readFileSync(new URL('../luch@toxblh.github.io/extension.js', import.meta.url), 'utf8');
const luchd = readFileSync(new URL('../../luchd/src/main.rs', import.meta.url), 'utf8');

test('every method the menu calls is one luchd serves', () => {
    const methods = [...ext.matchAll(/<method name="(\w+)"/g)].map(m => m[1]);
    assert.ok(methods.includes('Sleep') && methods.includes('Cast') && methods.includes('StopCast'));
    for (const m of methods) {
        const snake = m.replace(/[A-Z]/g, (c, i) => (i ? '_' : '') + c.toLowerCase());
        assert.match(luchd, new RegExp(`async fn ${snake}\\(`), m);
    }
});

test('a D-Bus error is shown without its prefix', () => {
    const body = ext.match(/function reason\(e\) \{([\s\S]*?)\n\}/)[1];
    const reason = new Function('e', body);
    assert.equal(reason(new Error('GDBus.Error:org.freedesktop.DBus.Error.Failed: sleep is off on the receiver')),
        'sleep is off on the receiver');
});

test('TVs without Luch are told by their id', () => {
    const isTv = new Function('id', ext.match(/function isTv\(id\) \{([\s\S]*?)\n\}/)[1]);
    assert.ok(isTv('cast:1c4f3a2b9d8e7f60a1b2c3d4e5f60718'));
    assert.ok(isTv('dlna:5f1c2e3d-0000-1000-8000-0123456789ab'));
    assert.ok(!isTv('3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47'));
});
