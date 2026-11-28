// Showing on several TVs: the dialog's ready choices become the right MirrorGroup call, and a playing group reads in
// plain words with Remove and Add.
import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import * as G from '../luch@toxblh.github.io/group.js';

const en = G.words(false), ru = G.words(true);
const tvs = [['a1', 'Living room'], ['b2', 'Kitchen'], ['c3', 'Bedroom']];

test('only receivers with Luch can play together', () => {
    const rows = [['a1', 'Living room'], ['cast:1c4f', 'Chromecast'], ['dlna:5f1c', 'Old TV'],
        ['web-3f0c', 'Web'], ['b2', 'Kitchen']];
    assert.deepEqual(G.candidates(rows), [['a1', 'Living room'], ['b2', 'Kitchen']]);
});

test('every TV is ticked to begin with, and this screen goes to all of them, in step', () => {
    const c = new G.Choice(tvs, en);
    assert.deepEqual(c.result(), {ids: ['a1', 'b2', 'c3'], roles: ['both', 'both', 'both'], source: 'fast',
        mode: 'watch'});
    assert.deepEqual(c.view().rows.map(r => r.picture), [null, null, null], 'no roles shown');
});

test('music in every room: sound only, every TV a speaker, always in step', () => {
    const c = new G.Choice(tvs, en);
    c.setPreset('music');
    c.setTiming('interactive');
    assert.deepEqual(c.result(), {ids: ['a1', 'b2', 'c3'], roles: ['audio', 'audio', 'audio'],
        source: 'audio', mode: 'watch'});
    assert.equal(c.view().timing, null, 'no timing to choose');
});

test('screen here, sound there: the chosen TV leads with the picture, the others are its speakers', () => {
    const c = new G.Choice(tvs, en);
    c.setPreset('split');
    assert.deepEqual(c.view().rows.map(r => r.picture), [true, false, false]);
    c.setPicture('c3');
    assert.deepEqual(c.result(), {ids: ['c3', 'a1', 'b2'], roles: ['video', 'audio', 'audio'], source: 'fast',
        mode: 'interactive'});
    // Its TV unticked: the first one still ticked takes the picture.
    c.tick('c3', false);
    assert.deepEqual(c.result().ids, ['a1', 'b2']);
    assert.equal(c.result().roles[0], 'video');
});

test('one TV is not a group: Show waits for a second one', () => {
    const c = new G.Choice(tvs, ru);
    c.tick('b2', false);
    c.tick('c3', false);
    assert.equal(c.view().ok, false);
    assert.equal(c.view().error, ru.tickTwo);
    assert.equal(c.result(), null);
    c.tick('c3', true);
    assert.ok(c.view().ok);
});

test('the timing under More options changes the screen choices, not their defaults', () => {
    const c = new G.Choice(tvs, en);
    assert.equal(c.view().timing, 'watch');
    c.setTiming('interactive');
    assert.equal(c.result().mode, 'interactive');
    c.setPreset('split');
    assert.equal(c.result().mode, 'interactive');
});

const group = {id: 'g', mode: 'watch', d_video_ms: 240, d_audio_ms: 240, sound_only: false, members: [
    {id: 'a1', name: 'Living room', role: 'both', present: true},
    {id: 'b2', name: 'Kitchen', role: 'audio', present: true}]};

test('a playing group in plain words: who, how far behind, Remove and Add', () => {
    assert.equal(G.title(group, en), 'Together: Living room, Kitchen');
    assert.equal(G.delayLine(group, en), 'All in step, 0.24 s behind this screen');
    assert.equal(G.delayLine(group, ru), 'Все в такт, отстают от экрана на 0,24 с');
    assert.equal(G.delayLine({...group, d_audio_ms: 0}, en), en.starting);
    assert.equal(G.delayLine({...group, mode: 'interactive', d_video_ms: 0, d_audio_ms: 2}, en),
        'Picture and sound right away');
    assert.equal(G.delayLine({...group, mode: 'interactive', d_video_ms: 0, d_audio_ms: 34}, en),
        'Picture right away, sound waits 30 ms for it');
    assert.deepEqual(G.removals(group, en), [['a1', 'Remove Living room (picture and sound)'],
        ['b2', 'Remove Kitchen (sound)']]);
    assert.deepEqual(G.addable([...tvs, ['dlna:x', 'Old TV']], group), [['c3', 'Bedroom']]);
});

test('the group is read from Stats, and the menu is built again only when its members change', () => {
    assert.equal(G.groupOf('{}'), null);
    assert.equal(G.groupOf('not json'), null);
    const g = G.groupOf(JSON.stringify({est_ms: 20, group}));
    assert.equal(g.members.length, 2);
    assert.ok(G.sameMembers(g, {...group, d_audio_ms: 260}));
    assert.ok(!G.sameMembers(g, {...group, members: group.members.slice(1)}));
    assert.ok(!G.sameMembers(g, null));
    assert.ok(G.sameMembers(null, null));
});

test('the Russian and the English words are the same set, without jargon', () => {
    const keys = t => Object.keys(t).sort();
    assert.deepEqual(keys(ru), keys(en));
    for (const t of [ru, en]) {
        const texts = v => typeof v === 'string' ? [v]
            : typeof v === 'function' ? [v.name === 'together' ? v(['X']) : v('X', 'Y')]
                : Object.values(v).flatMap(texts);
        const all = texts(t).join(' ');
        assert.doesNotMatch(all, /\brole\b|interactive|watch\b|group|sync|latency|codec|кодек|групп|синхрон/i);
    }
});

test('the methods the menu calls for groups are luchd’s, with the arguments it takes', () => {
    const ext = readFileSync(new URL('../luch@toxblh.github.io/extension.js', import.meta.url), 'utf8');
    const luchd = readFileSync(new URL('../../luchd/src/main.rs', import.meta.url), 'utf8');
    for (const m of ['MirrorGroup', 'AddToGroup', 'RemoveFromGroup'])
        assert.match(ext, new RegExp(`<method name="${m}">`));
    assert.match(luchd, /async fn add_to_group\(\s*&self,\s*member: String,\s*id: String,\s*role: String,/);
    assert.match(luchd, /async fn remove_from_group\(\s*&self,\s*id: String,/);
});
