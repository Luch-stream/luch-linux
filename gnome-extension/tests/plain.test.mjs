// luchd's refusals in plain words (plain.js), and the menu's words: the same in both languages, no jargon where
// everyone looks. problems.json is shared with the Mac app's PlainTests, so both senders tell the same trouble alike.
import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {problemKind, plainProblem, KNOWN_KINDS, PROBLEM_WORDS} from '../luch@toxblh.github.io/plain.js';

const vectors = JSON.parse(readFileSync(new URL('./problems.json', import.meta.url), 'utf8'));
const ext = readFileSync(new URL('../luch@toxblh.github.io/extension.js', import.meta.url), 'utf8');

test('each refusal is told as the kind it is', () => {
    for (const [text, kind] of vectors)
        assert.equal(problemKind(text), kind, text);
});

test('every kind has words in Russian and in English, and the cancelled one none', () => {
    for (const kind of KNOWN_KINDS.filter(k => k !== 'cancelled')) {
        assert.ok(PROBLEM_WORDS.ru[kind], `ru ${kind}`);
        assert.ok(PROBLEM_WORDS.en[kind], `en ${kind}`);
    }
    assert.equal(plainProblem("portal: Portal request didn't succeed: Cancelled", true), null);
});

test('a known refusal says what to do; an unknown one is shown as it is', () => {
    assert.match(plainProblem('receiver went away', true), /включён/);
    assert.match(plainProblem('receiver went away', false), /is on/);
    assert.equal(plainProblem('no document on that receiver', false), 'no document on that receiver');
    assert.equal(plainProblem(undefined, false), '');
});

/** The two word tables of extension.js, evaluated (their functions are only defined, not called). */
function tables() {
    const body = ext.match(/const T = RU \? \{([\s\S]*?)\n\} : \{([\s\S]*?)\n\};/);
    const make = src => new Function('reason', `return {${src}};`)(String);
    return {ru: make(body[1]), en: make(body[2])};
}

test('the Russian and the English menus have the same words', () => {
    const {ru, en} = tables();
    assert.deepEqual(Object.keys(ru).sort(), Object.keys(en).sort());
    assert.deepEqual(Object.keys(ru.quietModes).sort(), ['ask', 'off', 'on']);
});

test('the ways to show the screen speak no jargon', () => {
    const {ru, en} = tables();
    for (const t of [ru, en]) {
        for (const key of ['fast', 'monitor', 'window', 'extend', 'audio', 'stop', 'header', 'noDaemon'])
            assert.doesNotMatch(t[key], /portal|портал|latency|задержк|codec|кодек|luchd|mDNS|desktop|рабоч/i, key);
    }
});

test('the TV is asked with the word on its button', () => {
    // luch-android's pair_allow, in values-ru and values.
    const {ru, en} = tables();
    assert.ok(ru.allowOnTv('TV').includes('«Разрешить»'));
    assert.ok(en.allowOnTv('TV').includes('Allow'));
    assert.ok(PROBLEM_WORDS.ru.refused.includes('«Разрешить»'));
});
