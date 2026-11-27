// Do not disturb: the extension asks once a login, and gives the answer again when luchd asks again.
import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import QuietAsker from '../luch@toxblh.github.io/quiet.js';

const T = {quietTitle: 'Hide?', quietBody: 'body', quietHide: 'Hide them', quietKeep: 'Keep them'};

function fixture() {
    const answers = [], questions = [];
    const proxy = {AnswerQuietAsync: hide => { answers.push(hide); return Promise.resolve(); }};
    const asker = new QuietAsker(proxy, (title, body, actions) => questions.push({title, actions}), T);
    return {asker, answers, questions};
}

test('asks once, with two answers, and passes the one chosen to luchd', () => {
    const f = fixture();
    f.asker.onAsk();
    f.asker.onAsk(); // a second session starts while the question is open
    assert.equal(f.questions.length, 1);
    assert.deepEqual(f.questions[0].actions.map(a => a[0]), ['Hide them', 'Keep them']);
    f.questions[0].actions[0][1]();
    assert.deepEqual(f.answers, [true]);
});

test('after the answer luchd gets it again without a question (it forgets it when restarted)', () => {
    const f = fixture();
    f.asker.onAsk();
    f.questions[0].actions[1][1]();
    f.asker.onAsk();
    assert.equal(f.questions.length, 1);
    assert.deepEqual(f.answers, [false, false]);
});

test('the signal the extension listens to is one luchd sends', () => {
    const ext = readFileSync(new URL('../luch@toxblh.github.io/extension.js', import.meta.url), 'utf8');
    const luchd = readFileSync(new URL('../../luchd/src/main.rs', import.meta.url), 'utf8');
    for (const [, name] of ext.matchAll(/<signal name="(\w+)"/g)) {
        const snake = name.replace(/[A-Z]/g, (c, i) => (i ? '_' : '') + c.toLowerCase());
        assert.match(luchd, new RegExp(`async fn ${snake}\\(`), name);
    }
    for (const [, name] of ext.matchAll(/<property name="(\w+)"/g)) {
        const snake = name.replace(/[A-Z]/g, (c, i) => (i ? '_' : '') + c.toLowerCase());
        assert.match(luchd, new RegExp(`async fn ${snake}\\(`), name);
    }
});
