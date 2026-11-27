#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Fake Luch receiver for testing a sender without a TV.

Answers hello/welcome and start/ready on the control port, reassembles the UDP shards, validates the
24-byte header, Annex-B framing and the SPS/PPS-before-IDR rule, writes the elementary stream and finally
asks ffmpeg whether it decodes. This is `luch-android/tools/testsend.py` in reverse.

    Tools/fakerecv.py [--port 47800] [--seconds 15] [--out .tmp/capture.h264] [--bonjour]
"""
import argparse
import atexit
import json
import os
import socket
import struct
import subprocess
import sys
import threading
import time

HEADER = 24
KIND_VIDEO, KIND_AUDIO = 1, 2
FLAG_IDR, FLAG_CONFIG, FLAG_PARITY = 1, 2, 4


class Session:
    def __init__(self, seconds, out):
        self.seconds = seconds
        self.out = out
        self.video = None
        self.frames = 0
        self.bytes = 0
        self.lost = 0
        self.idr = 0
        self.bad = []
        self.frames_data = []
        self.audio = None
        self.codec = "video/avc"
        self.audio_head = None
        self.audio_sizes = []
        self.first_pts = None
        self.started = time.monotonic()
        self.lock = threading.Lock()
        self.ended = threading.Event()
        self.dbg = 0

    def note(self, msg):
        print(f"[recv] {msg}", flush=True)

    def fail(self, msg):
        if len(self.bad) < 20:
            self.bad.append(msg)
        self.note(f"BAD: {msg}")

    def on_frame(self, data, flags, pts):
        self.frames += 1
        self.bytes += len(data)
        if flags & FLAG_IDR:
            self.idr += 1
        with self.lock:
            self.frames_data.append(data)
        if self.frames == 1:
            self.note(f"frame 1: {len(data)} bytes, head {data[:32].hex()}")
        if self.frames < 3 and not data.startswith(b"\x00\x00\x00\x01"):
            self.fail(f"frame {self.frames} does not start with a start code")
        nals = [n for n in annexb_nals(data) if n]
        if self.codec == "video/hevc":
            types = [(n[0] >> 1) & 0x3F for n in nals]
            irap = [i for i, t in enumerate(types) if 16 <= t <= 21]
            if flags & FLAG_IDR:
                missing = [t for t in (32, 33, 34) if t not in types]
                if missing:
                    self.fail(f"HEVC key frame #{self.idr} misses parameter sets {missing}: types {types[:8]}")
                elif not irap:
                    self.fail(f"HEVC key frame #{self.idr} has no IRAP NAL: types {types[:8]}")
                elif min(irap) < max(types.index(t) for t in (32, 33, 34)):
                    self.fail(f"HEVC key frame #{self.idr}: VPS/SPS/PPS do not precede the slice")
            else:
                if irap:
                    self.fail(f"non-key HEVC frame carries an IRAP: types {types[:8]}")
                if not types or types[0] not in list(range(0, 10)) + [35, 39]:
                    self.fail(f"HEVC P-frame starts with NAL type {types[:1]}")
        else:
            types = [n[0] & 0x1F for n in nals]
            if flags & FLAG_IDR:
                if 7 not in types:
                    self.fail(f"IDR #{self.idr} has no SPS (NAL 7): types {types[:6]}")
                elif 8 not in types:
                    self.fail(f"IDR #{self.idr} has no PPS (NAL 8): types {types[:6]}")
                elif 5 not in types:
                    self.fail(f"IDR #{self.idr} has no IDR slice (NAL 5): types {types[:6]}")
                elif types.index(5) < max(types.index(7), types.index(8)):
                    self.fail(f"IDR #{self.idr}: SPS/PPS do not precede the slice: types {types[:6]}")
            else:
                if 5 in types:
                    self.fail(f"non-IDR frame carries an IDR slice: types {types[:6]}")
                if not types or types[0] not in (1, 6, 9):
                    self.fail(f"P-frame starts with NAL type {types[:1]}")


def annexb_nals(data):
    """Raw NAL units of an Annex-B buffer (3- or 4-byte start codes)."""
    out = []
    i = 0
    n = len(data)
    while i < n:
        j = data.find(b"\x00\x00\x01", i)
        if j < 0:
            break
        start = j + 3
        if j > 0 and data[j - 1] == 0:
            start = j + 3  # 4-byte start code: the zero belongs to it
        k = data.find(b"\x00\x00\x01", start)
        out.append(data[start:k if k >= 0 else n])
        i = k if k >= 0 else n
    return out


def udp_loop(sock, session):
    pending = {}
    cur_seq = None
    sock.settimeout(0.2)
    while not session.ended.is_set():
        try:
            buf, _ = sock.recvfrom(4096)
        except socket.timeout:
            continue
        except OSError:
            break
        if len(buf) < HEADER:
            continue
        kind, flags, idx, shards, _parity, seq, pts, flen, host_lat, _r = struct.unpack(">BBHHHIIIHH", buf[:HEADER])
        if session.dbg < 6:
            session.dbg += 1
            print(f"[recv] pkt kind={kind} flags={flags} idx={idx} shards={shards} seq={seq} flen={flen} body={len(buf)-HEADER}", flush=True)
        if kind != KIND_VIDEO:
            continue
        body = buf[HEADER:]
        # All shards of one access unit carry the same seq; a new seq means a new frame, and if the
        # previous one never completed it was lost (the real receiver then asks for an IDR).
        if seq != cur_seq:
            if pending and cur_seq is not None and seq != cur_seq + 1:
                session.lost += 1
            elif pending:
                session.lost += 1
            pending = {}
            cur_seq = seq
        pending[idx] = body
        if len(pending) == shards and all(i in pending for i in range(shards)):
            data = b"".join(pending[i] for i in range(shards))[:flen]
            pending.clear()
            if len(data) != flen:
                session.fail(f"frame {seq}: got {len(data)} of {flen} bytes")
            session.on_frame(data, flags, pts)


def audio_loop(sock, session):
    sock.settimeout(0.2)
    while not session.ended.is_set():
        try:
            buf, _ = sock.recvfrom(4096)
        except socket.timeout:
            continue
        except OSError:
            break
        if len(buf) < HEADER:
            continue
        kind, flags, _idx, _shards, _parity, _seq, _pts, flen, _hl, _r = struct.unpack(">BBHHHIIIHH", buf[:HEADER])
        if kind != KIND_AUDIO:
            continue
        data = buf[HEADER:HEADER + min(flen, len(buf) - HEADER)]
        if flags & FLAG_CONFIG:
            session.audio_head = data
            print(f"[recv] audio config: {len(data)} bytes head {data[:12].hex()}", flush=True)
        else:
            session.audio_sizes.append(len(data))


def stats_loop(conn, session):
    last_frames = 0
    last_bytes = 0
    last_t = time.monotonic()
    while not session.ended.is_set():
        time.sleep(1)
        now = time.monotonic()
        dt = max(now - last_t, 1e-3)
        with session.lock:
            frames = session.frames
            nbytes = session.bytes
        df = frames - last_frames
        mbps = (nbytes - last_bytes) * 8 / 1e6 / dt
        last_frames, last_bytes, last_t = frames, nbytes, now
        print(f"[recv] {df:>3} fps  {mbps:6.1f} Mbit/s  idr {session.idr}  lost {session.lost}", flush=True)
        try:
            conn.sendall((json.dumps({
                "t": "stats", "decode_ms": 5.0, "host_ms": 0, "fps": df,
                "lost": session.lost, "dropped": 0, "shown_seq": frames, "decoder": "fake",
            }) + "\n").encode())
        except OSError:
            return


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=47800)
    ap.add_argument("--seconds", type=float, default=15)
    ap.add_argument("--out", default=".tmp/capture.h264")
    ap.add_argument("--bonjour", action="store_true", help="also publish _luch._tcp with dns-sd")
    ap.add_argument("--name", default="FakeTV")
    args = ap.parse_args()

    session = Session(args.seconds, args.out)
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("0.0.0.0", args.port))
    srv.listen(1)
    srv.settimeout(args.seconds)
    print(f"[recv] control on :{args.port}, waiting for a sender (up to {args.seconds} s)", flush=True)

    if args.bonjour:
        # dns-sd wants one argument per TXT entry, not one long string.
        txt = ["v=0", "id=fake", f"name={args.name}", "model=spike", "dec=avc"]
        reg = subprocess.Popen(["dns-sd", "-R", args.name, "_luch._tcp", "local", str(args.port)] + txt,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        atexit.register(reg.terminate)

    try:
        conn, peer = srv.accept()
    except socket.timeout:
        print("[recv] no sender connected", flush=True)
        sys.exit(1)
    print(f"[recv] sender {peer[0]}:{peer[1]}", flush=True)
    conn.settimeout(0.2)

    def send(obj):
        conn.sendall((json.dumps(obj) + "\n").encode())

    deadline = time.monotonic() + args.seconds
    reader_done = threading.Event()

    def handle(msg):
        t = msg.get("t")
        if t == "hello":
            send({"t": "welcome", "v": 0, "id": "fake", "name": "Fake TV", "model": "spike",
                  "decoders": ["video/avc"], "max": "1920x1080", "modes": ["mirror"],
                  "hw": {"video": {"video/avc": {"max": "1920x1080", "1080p60": True}}},
                  "bench": {"video/avc": {"ms": 5.0, "frames": 300, "decoder": "fake"}, "prefer": "video/avc"}})
            print("[recv] hello", flush=True)
        elif t == "start":
            session.codec = (msg.get("video") or {}).get("codec", "video/avc")
            v = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            v.bind(("0.0.0.0", 0))
            v.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4 << 20)
            session.video = v
            threading.Thread(target=udp_loop, args=(v, session), daemon=True).start()
            a = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            a.bind(("0.0.0.0", 0))
            session.audio = a
            threading.Thread(target=audio_loop, args=(a, session), daemon=True).start()
            send({"t": "ready", "video_port": v.getsockname()[1], "audio_port": a.getsockname()[1]})
            print(f"[recv] start {msg.get('video')} -> video :{v.getsockname()[1]}", flush=True)
            time.sleep(0.3)
            send({"t": "keyframe"})
            threading.Thread(target=stats_loop, args=(conn, session), daemon=True).start()
        elif t == "sender_stats":
            print(f"[recv] sender_stats {msg}", flush=True)
        elif t in ("stop", "bye"):
            print(f"[recv] {t}", flush=True)
            session.ended.set()

    def reader():
        buf = b""
        try:
            while not session.ended.is_set():
                try:
                    chunk = conn.recv(65536)
                except socket.timeout:
                    continue          # the sender pings once a second; silence is normal
                except OSError:
                    break
                if not chunk:
                    break
                buf += chunk
                while b"\n" in buf:
                    raw, buf = buf.split(b"\n", 1)
                    if raw.strip():
                        handle(json.loads(raw))
        except Exception as e:  # noqa: BLE001 - a test tool
            print(f"[recv] control closed: {e}", flush=True)
        finally:
            session.ended.set()
            reader_done.set()

    threading.Thread(target=reader, daemon=True).start()
    while time.monotonic() < deadline and not session.ended.is_set():
        time.sleep(0.1)
    session.ended.set()

    # Give the UDP loop a moment to flush the last frame.
    time.sleep(0.3)
    data = b""
    with session.lock:
        data = b"".join(session.frames_data)
    os.makedirs(os.path.dirname(args.out) or ".", exist_ok=True)
    with open(args.out, "wb") as f:
        f.write(data)
    print(f"[recv] {session.frames} frames, {session.bytes} bytes, {session.idr} IDR, {session.lost} lost -> {args.out}", flush=True)
    if session.audio_head:
        head = session.audio_head
        ok = head[:8] == b"OpusHead"
        rate = int.from_bytes(head[12:16], "little") if len(head) >= 16 else 0
        print(f"[recv] opus head ok={ok} rate={rate} pre-skip={int.from_bytes(head[10:12], 'little') if len(head) >= 12 else 0}", flush=True)
    sizes = session.audio_sizes
    if sizes:
        print(f"[recv] audio packets: {len(sizes)}, bytes min/avg/max "
              f"{min(sizes)}/{sum(sizes)//len(sizes)}/{max(sizes)}", flush=True)
    else:
        print("[recv] no audio packets", flush=True)

    if session.bad:
        print("[recv] PROBLEMS:", flush=True)
        for b in session.bad:
            print("   -", b, flush=True)
    else:
        print("[recv] header/Annex-B/SPS-PPS checks passed", flush=True)

    if data:
        fmt = "hevc" if session.codec == "video/hevc" else "h264"
        probe = subprocess.run(
            ["ffmpeg", "-v", "error", "-f", fmt, "-i", args.out, "-f", "null", "-"],
            capture_output=True, text=True)
        if probe.returncode == 0 and not probe.stderr.strip():
            print("[recv] ffmpeg decoded the stream cleanly", flush=True)
        else:
            print("[recv] ffmpeg problems:", probe.stderr.strip()[:800], flush=True)
    sys.exit(0 if not session.bad and data else 1)


if __name__ == "__main__":
    main()
