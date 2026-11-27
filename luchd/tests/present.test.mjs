// The presenter view's logic (luchd/src/present.js): keys of a keyboard and a clicker, coordinates, the pointer and
// ink as they go to luchd, the timer.
import {test} from 'node:test';
import assert from 'node:assert/strict';
import {keyAction, targetPage, normalize, contentRect, formatClock, Timer, PointerCoalescer, StrokeSender,
    InkPages, drawStrokes} from '../src/present.js';

/** A clock the test moves, and timers that fire when it passes them. */
function fakeTime() {
    let t = 0;
    const timers = [];
    return {
        now: () => t,
        later: (fn, ms) => timers.push({at: t + ms, fn}),
        advance(ms) {
            const end = t + ms;
            for (;;) {
                timers.sort((a, b) => a.at - b.at);
                if (!timers.length || timers[0].at > end) break;
                const next = timers.shift();
                t = next.at;
                next.fn();
            }
            t = end;
        },
    };
}

test('a clicker turns slides, blanks the screen and its start button never leaves', () => {
    const k = key => keyAction({key});
    for (const key of ['PageDown', 'ArrowRight', ' ', 'Enter', 'n']) assert.equal(k(key), 'next', key);
    for (const key of ['PageUp', 'ArrowLeft', 'Backspace', 'p']) assert.equal(k(key), 'prev', key);
    assert.equal(k('b'), 'blank');
    assert.equal(k('.'), 'blank');
    assert.equal(k('F5'), 'unblank');
    assert.equal(k('Escape'), 'unblank');
    assert.equal(keyAction({key: 'z', ctrlKey: true}), 'undo');
    assert.equal(keyAction({key: 'r', ctrlKey: true}), null, 'the browser keeps its own shortcuts');
    assert.equal(k('q'), null);
});

test('page turns stay within the document', () => {
    assert.equal(targetPage('next', 0, 3), 1);
    assert.equal(targetPage('next', 2, 3), null, 'nothing after the last');
    assert.equal(targetPage('prev', 0, 3), null);
    assert.equal(targetPage('last', 0, 3), 2);
    assert.equal(targetPage('first', 2, 3), 0);
    assert.equal(targetPage('blank', 1, 3), null);
    assert.equal(targetPage('next', 0, 0), null, 'not loaded yet');
});

test('coordinates are fractions of the picture, not of the box around it', () => {
    // A 4:3 slide in a 1000x500 box: drawn 666.7 wide, centred.
    const r = contentRect({left: 0, top: 0, width: 1000, height: 500}, 800, 600);
    assert.deepEqual([Math.round(r.left), r.top, Math.round(r.width), r.height], [167, 0, 667, 500]);
    assert.deepEqual(normalize(500, 250, r), {x: 0.5, y: 0.5});
    assert.equal(normalize(100, 250, r), null, 'in the bar beside it');
    assert.deepEqual(normalize(r.left + r.width / 3, 0, r), {x: 0.3333, y: 0});
    assert.equal(normalize(1, 1, {left: 0, top: 0, width: 0, height: 0}), null);
});

test('pointer moves go out at most once a frame, the newest and the last always', () => {
    const time = fakeTime(), sent = [];
    const c = new PointerCoalescer(m => sent.push(m), {now: time.now, later: time.later});
    // A mouse at 1000 Hz for a second.
    for (let i = 0; i < 1000; i++) {
        c.offer({t: 'pointer', x: i / 1000, y: 0.5});
        time.advance(1);
    }
    time.advance(100);
    assert.ok(sent.length >= 55 && sent.length <= 65, `${sent.length} messages`);
    assert.equal(sent.at(-1).x, 0.999, 'the last position arrives');
    c.offer({t: 'pointer', show: false});
    assert.deepEqual(sent.at(-1), {t: 'pointer', show: false}, 'after a pause it goes at once');
});

test('a stroke goes out while it is drawn, in parts, without repeated points', () => {
    const time = fakeTime(), sent = [];
    const s = new StrokeSender(m => sent.push(m), {now: time.now, later: time.later});
    s.begin(7, {x: 0.1, y: 0.1}, '#e53935', 0.004);
    assert.deepEqual(sent[0], {t: 'ink', do: 'stroke', id: 7, color: '#e53935', width: 0.004, points: [0.1, 0.1]});
    for (let i = 1; i <= 100; i++) {
        s.add({x: 0.1 + i * 0.002, y: 0.1});
        s.add({x: 0.1 + i * 0.002, y: 0.1}); // the same point again adds nothing
        time.advance(3);
    }
    s.end();
    const parts = sent.slice(1);
    assert.ok(parts.length >= 9 && parts.length <= 12, `${parts.length} parts in 300 ms`);
    assert.ok(parts.every(p => p.id === 7 && p.color === undefined), 'colour and width only the first time');
    assert.equal(parts.reduce((n, p) => n + p.points.length, 0), 200, 'every new point once');
    s.end();
    assert.equal(sent.length, parts.length + 1, 'nothing more after the end');
});

test('a long fast stroke is cut into messages the receiver takes', () => {
    const sent = [];
    const s = new StrokeSender(m => sent.push(m), {now: () => 0, later: () => {}, max: 2000, minStep: 0});
    s.begin(1, {x: 0, y: 0}, '#fff', 0.004);
    for (let i = 0; i < 4500; i++) s.add({x: (i % 100) / 100, y: i / 4500});
    s.end();
    assert.ok(sent.every(m => m.points.length <= 4000));
    assert.equal(sent.slice(1).reduce((n, m) => n + m.points.length / 2, 0), 4500);
});

test('ink is kept per page, undo and clear act on the page shown', () => {
    const ink = new InkPages();
    ink.add(0, {points: [0, 0]});
    ink.add(0, {points: [1, 1]});
    ink.add(1, {points: [2, 2]});
    ink.undo(0);
    assert.equal(ink.strokes(0).length, 1);
    ink.clear(1);
    assert.equal(ink.strokes(1).length, 0);
    assert.equal(ink.strokes(0).length, 1, 'other pages keep theirs');
});

test('strokes are drawn in the picture, scaled with it', () => {
    const calls = [];
    const ctx = new Proxy({}, {
        get: (o, k) => (k in o ? o[k] : (...a) => calls.push([k, ...a])),
        set: (o, k, v) => { o[k] = v; return true; },
    });
    drawStrokes(ctx, [{color: '#e53935', width: 0.01, points: [0, 0, 0.5, 0.5, 1, 1]}],
        {left: 100, top: 0, width: 800, height: 600});
    assert.equal(ctx.lineWidth, 8);
    assert.deepEqual(calls.find(c => c[0] === 'moveTo'), ['moveTo', 100, 0]);
    assert.deepEqual(calls.find(c => c[0] === 'lineTo'), ['lineTo', 900, 600]);
});

test('the timer counts, pauses and resets', () => {
    let t = 0;
    const timer = new Timer(() => t);
    t = 65_000;
    assert.equal(formatClock(timer.elapsed()), '1:05');
    timer.toggle();
    t = 125_000;
    assert.equal(formatClock(timer.elapsed()), '1:05', 'paused');
    timer.toggle();
    t = 3_725_000 + 60_000;
    assert.equal(formatClock(timer.elapsed()), '1:02:05');
    timer.reset();
    assert.equal(formatClock(timer.elapsed()), '0:00');
});
