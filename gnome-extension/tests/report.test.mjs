// Report a problem: the form, luchd's Report with the person's choice, then the file, the description and the issue
// at a press each, and Send to the developers when luchd can send; nothing happens by itself.
import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import Reporter from '../luch@toxblh.github.io/report.js';

const T = {reportShow: 'Show the file', reportCopy: 'Copy description', reportIssue: 'Open an issue',
    reportSend: 'Send', reportSending: 'Sending…', reportSent: n => `Sent ${n}`, reportSentNoFile: n => `Sent ${n}, no file`,
    reportNotSent: e => `Not sent: ${e.message}`, reportCopyNumber: 'Copy the number'};
const REPORT = {path: '/home/someone/Downloads/Luch-report-2026-01-02-0304.zip', description: 'What happened: x',
    issue: 'https://example.org/issues/new?title=x', notes: []};

function fixture(answer, reply = Promise.resolve([JSON.stringify(REPORT)]), sendReply = null) {
    const calls = [], shown = [], busy = [], failures = [], desk = [], sent = [], said = [];
    const proxy = {
        ReportAsync: (text, keep) => { calls.push([text, keep]); return reply; },
        SendReportAsync: path => { sent.push(path); return sendReply; },
    };
    const ui = {
        form: () => Promise.resolve(answer),
        busy: on => busy.push(on),
        done: (r, actions) => {
            shown.push({r, actions});
            return {say: (text, list) => said.push({text, labels: list.map(a => a[0]), list})};
        },
        failed: e => failures.push(String(e)),
    };
    const os = {showFile: p => desk.push(['show', p]), copy: t => desk.push(['copy', t]), open: u => desk.push(['open', u])};
    return {reporter: new Reporter(proxy, ui, os, T), calls, shown, busy, failures, desk, sent, said};
}

const SENDABLE = Promise.resolve([JSON.stringify({...REPORT, can_send: true})]);
const pressSend = async f => {
    await f.reporter.run();
    const send = f.shown[0].actions.find(a => a[0] === 'Send');
    await send[1]();
};

test('the person’s text and choice go to luchd, the three actions only on a press', async () => {
    const f = fixture({text: '  HEVC stutters \n', hide: true});
    await f.reporter.run();
    assert.deepEqual(f.calls, [['HEVC stutters', false]], 'hidden names: keep_names false');
    assert.deepEqual(f.busy, [true, false]);
    assert.equal(f.shown.length, 1);
    assert.deepEqual(f.shown[0].actions.map(a => a[0]), ['Show the file', 'Copy description', 'Open an issue']);
    assert.deepEqual(f.desk, [], 'nothing is opened or copied by itself');
    for (const [, press] of f.shown[0].actions)
        press();
    assert.deepEqual(f.desk, [['show', REPORT.path], ['copy', REPORT.description], ['open', REPORT.issue]]);
});

test('names kept when the box is cleared; a cancelled form asks luchd nothing', async () => {
    const kept = fixture({text: 'x', hide: false});
    await kept.reporter.run();
    assert.deepEqual(kept.calls, [['x', true]]);
    const cancelled = fixture(null);
    await cancelled.reporter.run();
    assert.deepEqual(cancelled.calls, []);
    assert.equal(cancelled.shown.length, 0);
});

test('Send only when luchd can send; it shows Sending…, then the number with Copy', async () => {
    const f = fixture({text: 'x', hide: true}, SENDABLE,
        Promise.resolve([JSON.stringify({number: 'LUCH-7F3C0A1B', event_id: '7f3c0a1b', attached: true})]));
    await f.reporter.run();
    assert.deepEqual(f.shown[0].actions.map(a => a[0]), ['Show the file', 'Copy description', 'Open an issue', 'Send']);
    assert.deepEqual(f.sent, [], 'nothing is sent before the press');
    await f.shown[0].actions[3][1]();
    assert.deepEqual(f.sent, [REPORT.path]);
    assert.deepEqual(f.said.map(s => [s.text, s.labels]), [['Sending…', []], ['Sent LUCH-7F3C0A1B', ['Copy the number']]]);
    f.said[1].list[0][1]();
    assert.deepEqual(f.desk, [['copy', 'LUCH-7F3C0A1B']]);
});

test('a file the server did not take is told with the number', async () => {
    const f = fixture({text: 'x', hide: true}, SENDABLE,
        Promise.resolve([JSON.stringify({number: 'LUCH-7F3C0A1B', attached: false, attach_error: 'HTTP 400'})]));
    await pressSend(f);
    assert.equal(f.said[1].text, 'Sent LUCH-7F3C0A1B, no file');
});

test('a failed send says why and keeps the file’s ways, Send again among them', async () => {
    const f = fixture({text: 'x', hide: true}, SENDABLE, Promise.reject(new Error('cannot reach the developers’ server')));
    await pressSend(f);
    assert.deepEqual(f.said.map(s => [s.text, s.labels]), [['Sending…', []],
        ['Not sent: cannot reach the developers’ server', ['Show the file', 'Copy description', 'Open an issue', 'Send']]]);
    assert.equal(f.reporter.sending, false);
    f.said[1].list[0][1]();
    assert.deepEqual(f.desk, [['show', REPORT.path]]);
});

test('a refusal is told, and the next press starts again', async () => {
    const f = fixture({text: '', hide: true}, Promise.reject(new Error('cannot write the report: disk full')));
    await f.reporter.run();
    assert.deepEqual(f.failures, ['Error: cannot write the report: disk full']);
    assert.equal(f.reporter.running, false);
    assert.deepEqual(f.busy, [true, false]);
});

test('a second press while collecting does not start another', async () => {
    let finish;
    const f = fixture({text: 'x', hide: true}, new Promise(r => { finish = r; }));
    const first = f.reporter.run();
    await f.reporter.run();
    finish([JSON.stringify(REPORT)]);
    await first;
    assert.equal(f.calls.length, 1);
});

test('Report is what luchd serves, with the arguments the extension passes', () => {
    const ext = readFileSync(new URL('../luch@toxblh.github.io/extension.js', import.meta.url), 'utf8');
    const luchd = readFileSync(new URL('../../luchd/src/main.rs', import.meta.url), 'utf8');
    assert.match(ext, /<method name="Report"><arg name="text" type="s" direction="in"\/><arg name="keep_names" type="b" direction="in"\/><arg type="s" direction="out"\/><\/method>/);
    assert.match(luchd, /async fn report\(&self, text: String, keep_names: bool\) -> fdo::Result<String>/);
    assert.match(ext, /<method name="SendReport"><arg name="path" type="s" direction="in"\/><arg type="s" direction="out"\/><\/method>/);
    assert.match(luchd, /async fn send_report\(&self, path: String\) -> fdo::Result<String>/);
});
