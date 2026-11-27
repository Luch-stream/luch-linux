// Manager: N fake receivers for SECS seconds, prints their ids as they register.
import { spawn } from 'node:child_process';
const N = Number(process.argv[2] ?? 8), SECS = Number(process.argv[3] ?? 90);
const kids = [], ids = [];
let registered = 0;
for (let i = 0; i < N; i++) {
  const k = spawn('bun', ['udp-fake.mjs', String(i)], { stdio: ['ignore', 'pipe', 'inherit'] });
  kids.push(k);
  k.stdout.on('data', d => {
    for (const line of d.toString().split('\n')) {
      if (!line.trim()) continue;
      let j; try { j = JSON.parse(line); } catch { continue; }
      if (j.id && !j.udp_datagrams) {
        ids.push(j.id); registered++;
        console.log('REG ' + j.id);
      } else if (j.udp_datagrams !== undefined) console.log('STATS ' + line);
    }
  });
}
setTimeout(() => { for (const k of kids) k.kill('SIGTERM'); setTimeout(() => process.exit(0), 2000); }, SECS * 1000);
