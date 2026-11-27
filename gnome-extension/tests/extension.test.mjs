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

test('the menu says what the discovery ladder tries', () => {
    const body = ext.match(/function findLine\(json, t\) \{([\s\S]*?)\n\}/)[1];
    const findLine = new Function('json', 't', body);
    const t = {rungs: {ble: 'Bluetooth', scan: 'every address'}, trying: w => `Looking: ${w}…`};
    const status = {running: true, rungs: [{rung: 'mdns', state: 'done'}, {rung: 'scan', state: 'running'},
        {rung: 'ble', state: 'running'}]};
    assert.equal(findLine(JSON.stringify(status), t), 'Looking: every address, Bluetooth…');
    assert.equal(findLine(JSON.stringify({...status, running: false}), t), null);
    assert.equal(findLine('not json', t), null);
    assert.ok(/<signal name="FindProgress">/.test(ext));
    assert.match(luchd, /async fn find_progress\(/);
});
