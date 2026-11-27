// Fake Android-TV-style receiver: Luch v1 control handshake over TCP, two UDP ports that
// count what the sender delivers. Published via Avahi so luchd discovers it.
import net from 'node:net';
import dgram from 'node:dgram';
import { spawn } from 'node:child_process';

const idx = process.argv[2] ?? '0';
const name = `Luch fake ${idx}`;
const id = crypto.randomUUID();

const v = dgram.createSocket('udp4'), a = dgram.createSocket('udp4');
await new Promise(r => v.bind(0, '0.0.0.0', r));
await new Promise(r => a.bind(0, '0.0.0.0', r));
const st = { datagrams: 0, bytes: 0, max_seq: -1, audio_shards: 0 };
v.on('message', m => {
  st.datagrams++; st.bytes += m.length;
  if (m.length >= 24) { const s = m.readUInt32BE(8); if (s > st.max_seq) st.max_seq = s; }
});
a.on('message', m => { st.audio_shards++; });

const server = net.createServer(sock => {
  let buf = '';
  sock.on('data', d => {
    buf += d.toString('utf8');
    for (let i; (i = buf.indexOf('\n')) >= 0;) {
      const line = buf.slice(0, i); buf = buf.slice(i + 1);
      let m; try { m = JSON.parse(line); } catch { continue; }
      if (m.t === 'hello') {
        const modes = (process.env.FAKE_MODES ?? 'mirror').split(',').map(x => x.trim()).filter(Boolean);
        sock.write(JSON.stringify({ t: 'welcome', v: 0, id, name, app: 'fake',
          bench: { prefer: 'video/avc' }, modes, hw: {} }) + '\n');
      }
      else if (m.t === 'start')
        sock.write(JSON.stringify({ t: 'ready', video_port: v.address().port, audio_port: a.address().port }) + '\n');
      else if (m.t === 'stop' || m.t === 'bye') sock.end();
      else if (m.t === 'ping') sock.write(JSON.stringify({ t: 'pong', n: m.n }) + '\n');
    }
  });
  sock.on('error', () => {});
});
const port = await new Promise(r => server.listen(0, '0.0.0.0', () => r(server.address().port)));
const avahi = spawn('avahi-publish-service', ['--no-reverse', name, '_luch._tcp', String(port),
  `id=${id}`, `name=${name}`, 'model=fake', 'dec=avc', 'v=0'], { stdio: 'ignore' });
console.log(JSON.stringify({ id, port, fake: idx }));

const fin = () => {
  console.log(JSON.stringify({ id, fake: idx, udp_datagrams: st.datagrams, udp_bytes: st.bytes,
    max_seq: st.max_seq, audio_shards: st.audio_shards }));
  try { avahi.kill('SIGTERM'); } catch {}
  process.exit(0);
};
process.on('SIGTERM', fin); process.on('SIGINT', fin);
