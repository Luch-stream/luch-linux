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

test('a refusal that asks for pairing is told from any other', () => {
    const body = ext.match(/function asksForPairing\(text\) \{([\s\S]*?)\n\}/)[1];
    const asks = new Function('text', body);
    assert.ok(asks('Living room TV asks for pairing: enter the code it shows (`luchctl pair tv`)'));
    assert.ok(!asks('Living room TV refused the connection'));
});

test('the pairing signal is the one luchd sends', () => {
    assert.match(ext, /<signal name="PairChanged">/);
    assert.match(luchd, /async fn pair_changed\(e: &SignalEmitter<'_>, id: &str, state: &str\)/);
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

test('TVs without Luch are told by their id', () => {
    const isTv = new Function('id', ext.match(/function isTv\(id\) \{([\s\S]*?)\n\}/)[1]);
    assert.ok(isTv('cast:1c4f3a2b9d8e7f60a1b2c3d4e5f60718'));
    assert.ok(isTv('dlna:5f1c2e3d-0000-1000-8000-0123456789ab'));
    assert.ok(!isTv('3f0c2a8e-9d1b-4c7e-8a55-0b6f2d9e1c47'));
});

test('the menu follows a direct connection, and only while it is under way', () => {
    const directLine = new Function('json', 't', ext.match(/function directLine\(json, t\) \{([\s\S]*?)\n\}/)[1]);
    const t = {directSteps: {asking: 'allow the connection on the TV'},
        directing: (name, step) => `Directly to ${name}: ${step}…`};
    assert.equal(directLine(JSON.stringify({step: 'asking', name: 'Living room TV'}), t),
        'Directly to Living room TV: allow the connection on the TV…');
    for (const done of ['idle', 'on', 'back', 'failed'])
        assert.equal(directLine(JSON.stringify({step: done}), t), null, done);
    assert.equal(directLine('not json', t), null);
    assert.match(ext, /<signal name="DirectChanged">/);
    assert.match(luchd, /async fn direct_changed\(e: &SignalEmitter<'_>, status: &str\)/);
});
