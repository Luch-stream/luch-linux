# Group Engine load harness

Headless receivers for measuring the sender side of a group without real TVs and without
decoding on the sender's machine. They speak the same Luch v1 control protocol as a receiver,
so `luchd` cannot tell them from devices.

Requirements: [Bun](https://bun.sh), `avahi-publish-service` (for the UDP fake), a running
`luchd` (release build for meaningful numbers).

## WebSocket readers (no decode)

```
bun ws-reader.mjs <count> <seconds>        # wss://127.0.0.1:47801/ws
```

Each connection does `hello` → `welcome`, answers `start` with `ready`, and reads binary media
without decoding. Prints connections, started count, delivered frames, frames/s per receiver
and total egress. They create the group if none runs (the screen-cast portal must be approved
once) and join the single running group otherwise.

## UDP fake receivers

```
bun udp-fake.mjs <index>                   # prints {"id":...,"port":...}, SIGTERM prints totals
bun udp-fakes.mjs <count> <seconds>        # manages <count> fakes, prints REG <id> lines
```

Each fake publishes `_luch._tcp` via Avahi, listens on a TCP control port, answers
`hello`/`start` with `welcome`/`ready`, and binds two UDP ports for video and audio. Bind them
to a running group with D-Bus:

```
# group ids are g-<web receiver id>; list web receivers with Receivers and read GroupStats
GID=g-web-<receiver-uuid>
busctl --user call dev.luch.Luch1 /dev/luch/Luch1 dev.luch.Luch1 Connect s "$ID"
busctl --user call dev.luch.Luch1 /dev/luch/Luch1 dev.luch.Luch1 AddToGroup ss "$GID" "$ID"
```

On SIGTERM each fake prints received datagrams/bytes and the highest seq — all fakes in one
group must show identical counts.

## What the numbers mean

Read `GroupStats(s group_id)`: `capture_instances` and `encoder.live_instances` must stay 1 for
one group, `encoder.pacing_ms` is the real spacing of published video frames, and per receiver
`state`/`dropped`/`skipped`/`rate_mbps` show delivery. Sender CPU is the cheapest honest number:

```
PID=$(pgrep -x luchd)
python3 - "$PID" <<'PY'
import sys, time
pid = int(sys.argv[1])
def t():
    f = open(f'/proc/{pid}/stat').read().split()
    return int(f[13]) + int(f[14])
a = t(); time.sleep(3); b = t()
print((b - a) / 3 / 100, 'cores')
PY
```

## Notes

- Readers on the sender's own machine measure the fan-out, not a network: no Wi-Fi, no losses,
  and no device decode. The WebSocket path is what is measured; UDP is covered by the fakes.
- A 144 Hz screen with the default `LUCH_FPS=60` gives a uniform 48 fps stream (144/3).
  Only `0` (the screen's own rate), `60` and `30` are accepted; see the main README.
- Web browsers that decode several 1080p streams on the sender's machine compete with the
  encoder for CPU; that is not a property of a real deployment.
