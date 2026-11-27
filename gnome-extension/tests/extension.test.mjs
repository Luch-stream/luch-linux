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
