// The web receiver's picture: a decoder starts at a keyframe, is closed whenever it is let go, a hidden tab decodes
// nothing and comes back from a keyframe, and keyframes are asked for sparingly.
import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';

const script = readFileSync(new URL('../src/web.html', import.meta.url), 'utf8').split('<script>')[1].split('</script>')[0];

function receiver() {
  const events = {}, sent = [], decoders = [];
  const node = {style: {}, textContent: '', offsetParent: null, querySelector() {return this;}, focus() {}};
  let now = 10_000;
  class VideoDecoder {
    constructor(init) { this.init = init; this.state = 'unconfigured'; this.chunks = []; this.closed = false;
      this.decodeQueueSize = 0; decoders.push(this); }
    configure(c) { this.config = c; this.state = 'configured'; }
    decode(c) { this.chunks.push(c.type); }
    close() { this.closed = true; this.state = 'closed'; }
  }
  const context = vm.createContext({
    navigator: {language: 'en', userAgent: 'Test/1'}, screen: {width: 1920, height: 1080}, devicePixelRatio: 1,
    location: {search: ''}, URLSearchParams,
    window: {isSecureContext: true, VideoDecoder, AudioDecoder: class {}},
    VideoDecoder, EncodedVideoChunk: class { constructor(c) { Object.assign(this, c); } },
    document: {visibilityState: 'visible', body: {}, getElementById: () => node,
      addEventListener: (name, fn) => {events[name] = fn;}},
    addEventListener: (name, fn) => {events[name] = fn;}, setInterval() {}, console: {warn() {}, error() {}},
    Date: {now: () => now},
  });
  vm.runInContext(script, context);
  vm.runInContext('ws = {readyState: 1, send: s => sent.push(JSON.parse(s))}', Object.assign(context, {sent}));
  const run = s => vm.runInContext(s, context);
  // An IDR with an SPS (High profile, level 4.0), or a delta frame.
  const frame = key => run(`video({flags: ${key ? 1 : 0}}, new Uint8Array(${key
    ? '[0, 0, 1, 0x67, 0x64, 0x00, 0x28, 0xac, 0, 0, 1, 0x65, 1]' : '[0, 0, 1, 0x41, 1]'}), 0)`);
  const keyframes = () => sent.filter(m => m.t === 'keyframe').length;
  return {context, events, sent, decoders, run, frame, keyframes, later: ms => { now += ms; }};
}

test('a decoder takes a keyframe first: deltas before it are skipped and one keyframe is asked for', () => {
  const r = receiver();
  r.frame(false);
  assert.equal(r.decoders.length, 0, 'no SPS yet: no decoder');
  assert.equal(r.keyframes(), 1);
  r.frame(true);
  r.frame(false);
  assert.deepEqual(r.decoders[0].chunks, ['key', 'delta']);
  assert.equal(r.decoders[0].config.codec, 'avc1.640028');
});

test('a decoder that falls behind skips to the next keyframe', () => {
  const r = receiver();
  r.frame(true);
  r.decoders[0].decodeQueueSize = 9;
  r.frame(false);
  r.decoders[0].decodeQueueSize = 0;
  r.frame(false);
  assert.deepEqual(r.decoders[0].chunks, ['key', 'delta'], 'the delta after the backlog is not decoded');
  assert.equal(r.keyframes(), 1);
  r.frame(true);
  assert.deepEqual(r.decoders[0].chunks, ['key', 'delta', 'key']);
});

test('a hidden tab closes its decoder, decodes nothing, and starts again from a keyframe', async () => {
  const r = receiver();
  r.run('setStreaming(true)');
  r.frame(true);
  r.context.document.visibilityState = 'hidden';
  await r.events.visibilitychange();
  assert.ok(r.decoders[0].closed);
  r.frame(true);
  assert.equal(r.decoders.length, 1, 'nothing decoded while hidden');
  r.context.document.visibilityState = 'visible';
  await r.events.visibilitychange();
  assert.equal(r.keyframes(), 1, 'back: a keyframe at once');
  r.frame(false);
  r.frame(true);
  r.frame(false);
  assert.deepEqual(r.decoders[1].chunks, ['key', 'delta']);
});

test('a decoder is closed on a reset and on an error, and keyframes are asked at most every 2 s', () => {
  const r = receiver();
  r.frame(true);
  r.run('reset()');
  assert.ok(r.decoders[0].closed);
  r.frame(true);
  r.decoders[1].init.error(new Error('decode failed'));
  assert.ok(r.decoders[1].closed);
  assert.equal(r.keyframes(), 1);
  r.later(500);
  r.frame(false);
  assert.equal(r.keyframes(), 1, 'asked again within 2 s: not sent');
  r.later(2000);
  r.frame(false);
  assert.equal(r.keyframes(), 2);
});
