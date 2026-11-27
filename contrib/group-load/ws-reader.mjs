// Headless Luch web receivers: full control handshake, media read and discarded.
// Purpose: server-side fan-out load without decoding on the sender's machine.
const N = Number(process.argv[2] || 20), SECS = Number(process.argv[3] || 40);
const url = 'wss://127.0.0.1:47801/ws';
const conns = [];
for (let i = 0; i < N; i++) {
  const st = { frames: 0, bytes: 0, started: false, err: '', closed: '' };
  const ws = new WebSocket(url, { tls: { rejectUnauthorized: false } });
  ws.onopen = () => ws.send(JSON.stringify({
    t: 'hello', v: 0, name: 'load' + i, model: 'headless',
    display: { width: 640, height: 360 }, mode: 'monitor',
  }));
  ws.onmessage = e => {
    if (typeof e.data === 'string') {
      const m = JSON.parse(e.data);
      if (m.t === 'ping') ws.send(JSON.stringify({ t: 'pong', n: m.n }));
      if (m.t === 'start') { ws.send(JSON.stringify({ t: 'ready', video_port: 0, audio_port: 0 })); st.started = true; }
      if (m.t === 'error') st.err = m.msg || m.code || 'error';
    } else { st.frames++; st.bytes += (e.data.byteLength ?? e.data.length ?? 0); }
  };
  ws.onclose = () => { st.closed = 'closed'; };
  ws.onerror = () => { if (!st.err) st.err = 'ws error'; };
  conns.push({ ws, st });
}
setTimeout(() => {
  let frames = 0, bytes = 0, started = 0;
  for (const { st } of conns) { frames += st.frames; bytes += st.bytes; if (st.started) started++; }
  console.log(JSON.stringify({
    conns: conns.length, started, secs: SECS,
    frames, fps_per_conn: +(frames / SECS / conns.length).toFixed(1),
    egress_mbps: +(bytes * 8 / 1e6 / SECS).toFixed(1),
    errors: [...new Set(conns.map(c => c.st.err).filter(Boolean))].slice(0, 3),
  }));
  for (const { ws } of conns) try { ws.close(); } catch {}
  process.exit(0);
}, SECS * 1000);
