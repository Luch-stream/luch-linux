import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';

const script = readFileSync(new URL('../src/web.html', import.meta.url), 'utf8').split('<script>')[1].split('</script>')[0];
const flush = () => new Promise(resolve => setImmediate(resolve));
function receiver(request) {
  const events = {};
  const node = {style: {}, textContent: '', offsetParent: null, querySelector() {return this;}, focus() {}};
  const context = vm.createContext({
    navigator: {language: 'en', ...(request ? {wakeLock: {request}} : {})},
    window: {isSecureContext: true, VideoDecoder: class {}, AudioDecoder: class {}},
    document: {visibilityState: 'visible', body: {}, getElementById: () => node,
      addEventListener: (name, fn) => {events[name] = fn;}},
    addEventListener: (name, fn) => {events[name] = fn;}, setInterval() {}, console: {warn() {}},
  });
  vm.runInContext(script, context);
  return {context, events, run: s => vm.runInContext(s, context)};
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
  await r.events.visibilitychange(); assert.equal(locks.length, 1);
  r.context.document.visibilityState = 'visible'; await r.events.visibilitychange();
  assert.equal(locks.length, 2);
  r.run('setStreaming(false)'); await flush();
  await r.events.visibilitychange(); assert.equal(locks.length, 2);
});

test('unsupported or denied wake lock leaves playback usable', async () => {
  const unsupported = receiver(); unsupported.run('setStreaming(true)'); await flush();
  assert.equal(unsupported.run('streaming'), true);
  const denied = receiver(async () => {throw Error('Denied');});
  denied.run('setStreaming(true)'); await flush();
  assert.equal(denied.run('streaming'), true);
  assert.equal(denied.run('screenLockPending'), false);
});
