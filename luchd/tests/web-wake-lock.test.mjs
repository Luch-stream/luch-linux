import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';

const script = readFileSync(new URL('../src/web.html', import.meta.url), 'utf8').split('<script>')[1].split('</script>')[0];
const flush = () => new Promise(resolve => setImmediate(resolve));
function receiver(request, fit = {}) {
  const events = {};
  const node = {style: {}, textContent: '', offsetParent: null, querySelector() {return this;}, focus() {}};
  // A DOM event has many listeners: the wake lock and the playback code both listen to
  // visibilitychange, so the page must see every handler, not the last one registered.
  const listen = (name, fn) => {(events[name] ??= []).push(fn);};
  const VideoDecoder = fit.VideoDecoder ?? class {configure() {} decode() {} close() {}};
  const context = vm.createContext({
    navigator: {language: 'en', ...(request ? {wakeLock: {request}} : {})},
    window: {isSecureContext: true, VideoDecoder, AudioDecoder: class {}},
    VideoDecoder, EncodedVideoChunk: fit.EncodedVideoChunk ?? class {constructor(init) {Object.assign(this, init);}},
    document: {visibilityState: 'visible', body: {}, getElementById: () => node,
      addEventListener: listen},
    addEventListener: listen, setInterval() {}, console: {warn() {}, error() {}},
  });
  vm.runInContext(script, context);
  return {context, events, fire: async name => {for (const fn of events[name] ?? []) await fn();},
    run: s => vm.runInContext(s, context)};
}
function sentinel() {
  return {released: false, releases: 0, addEventListener(name, fn) {this.onrelease = fn;},
    async release() {this.released = true; this.releases++; this.onrelease?.();}};
}

test('stream holds one screen lock and releases it on stop', async () => {
  const lock = sentinel(); let requests = 0;
  const r = receiver(async type => {assert.equal(type, 'screen'); requests++; return lock;});
  r.run('setStreaming(true); setStreaming(true)'); await flush();
  assert.equal(requests, 1);
  assert.equal(r.run('screenLock.released'), false);
  r.run('setStreaming(false)'); await flush();
  assert.equal(lock.releases, 1);
  assert.equal(r.run('screenLock'), null);
});

test('stop while request is pending releases the late grant', async () => {
  let grant; const lock = sentinel();
  const r = receiver(() => new Promise(resolve => {grant = resolve;}));
  r.run('setStreaming(true); setStreaming(false)'); grant(lock); await flush();
  assert.equal(lock.releases, 1);
  assert.equal(r.run('screenLock'), null);
});

test('returning to visible streaming page reacquires an automatically released lock', async () => {
  const locks = [];
  const r = receiver(async () => {const lock = sentinel(); locks.push(lock); return lock;});
  r.run('setStreaming(true)'); await flush();
  r.context.document.visibilityState = 'hidden'; await locks[0].release();
  await r.fire('visibilitychange'); assert.equal(locks.length, 1);
  r.context.document.visibilityState = 'visible'; await r.fire('visibilitychange');
  assert.equal(locks.length, 2);
  r.run('setStreaming(false)'); await flush();
  await r.fire('visibilitychange'); assert.equal(locks.length, 2);
});

test('unsupported or denied wake lock leaves playback usable', async () => {
  const unsupported = receiver(); unsupported.run('setStreaming(true)'); await flush();
  assert.equal(unsupported.run('streaming'), true);
  const denied = receiver(async () => {throw Error('Denied');});
  denied.run('setStreaming(true)'); await flush();
  assert.equal(denied.run('streaming'), true);
  assert.equal(denied.run('screenLockPending'), false);
});

// One parameter-set key frame: SPS 67 42 00 1e, enough for codecOf() to build avc1.42001e.
const KEY = 'video({flags: 1}, new Uint8Array([0,0,1,0x67,0x42,0,0x1e,0,0,0]), 0)';

test('a hidden page closes its decoder, a visible one asks for a keyframe again', async () => {
  const made = [];
  const VideoDecoder = class {
    constructor() {this.state = 'unconfigured'; this.decodeQueueSize = 0; this.closed = 0; made.push(this);}
    configure() {this.state = 'configured';}
    decode() {}
    close() {this.closed++; this.state = 'closed';}
  };
  const sends = [];
  const r = receiver(null, {VideoDecoder});
  r.context.__sends = sends;
  r.run('streaming = true; ws = {readyState: 1, send: m => __sends.push(m)}');
  r.run(KEY);
  assert.equal(made.length, 1);
  r.context.document.visibilityState = 'hidden';
  await r.fire('visibilitychange');
  assert.equal(made[0].closed, 1);          // decoder released while the tab is throttled
  assert.equal(r.run('vdec'), null);
  r.context.document.visibilityState = 'visible';
  await r.fire('visibilitychange');
  assert.deepEqual(sends.map(m => JSON.parse(m).t), ['keyframe']);
});

test('a decoder that throws is dropped and does not break the page', () => {
  let closed = 0;
  const VideoDecoder = class {
    constructor() {this.state = 'configured'; this.decodeQueueSize = 0;}
    configure() {}
    decode() {throw Error('boom');}
    close() {closed++;}
  };
  const r = receiver(null, {VideoDecoder});
  r.run('streaming = true; ws = {readyState: 1, send() {}}');
  assert.doesNotThrow(() => r.run(KEY));
  assert.equal(r.run('vdec'), null);
  assert.equal(closed, 1);
});
