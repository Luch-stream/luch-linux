"""Backend of the Luch plugin: talks to luchd (session bus, dev.luch.Luch1) with busctl.

Decky runs plugins as the user, but the session bus address is not always in the environment,
so it is built from the user's runtime directory.
"""
import json
import os
import pwd
import subprocess

import decky

BUS = "dev.luch.Luch1"
PATH = "/dev/luch/Luch1"
IFACE = "dev.luch.Luch1"

SIZES = ["1280x720", "1920x1080", "2560x1440", "3840x2160"]
SETTINGS = os.path.join(decky.DECKY_PLUGIN_SETTINGS_DIR, "settings.json")


USER = decky.DECKY_USER or "deck"
UID = pwd.getpwnam(USER).pw_uid
RUNTIME = f"/run/user/{UID}"
SOCKET = f"{RUNTIME}/luch/kms.sock"
HELPER_DIR = os.path.join(decky.DECKY_PLUGIN_DIR, "helper")

_helper = None


def _busctl(*args, timeout=20):
    # The plugin runs as root (it starts the grabber); luchd lives on the user's session bus.
    cmd = ["runuser", "-u", USER, "--", "env", f"XDG_RUNTIME_DIR={RUNTIME}",
           f"DBUS_SESSION_BUS_ADDRESS=unix:path={RUNTIME}/bus", "busctl", "--user", "--json=short", *args]
    out = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    if out.returncode != 0:
        raise RuntimeError(out.stderr.strip() or "busctl failed")
    return json.loads(out.stdout) if out.stdout.strip() else {}


def _call(method, signature="", *values):
    return _busctl("call", BUS, PATH, IFACE, method, *([signature, *map(str, values)] if signature else []))


def _load():
    try:
        with open(SETTINGS) as f:
            return json.load(f)
    except (OSError, ValueError):
        return {}


def _save(data):
    os.makedirs(os.path.dirname(SETTINGS), exist_ok=True)
    with open(SETTINGS, "w") as f:
        json.dump(data, f)


class Plugin:
    async def state(self):
        """Everything the panel shows: receivers, sources, the saved choice per receiver, LocalAudio."""
        try:
            rows = _call("Receivers")["data"][0]
        except Exception as e:  # luchd not running, or no busctl
            return {"up": False, "error": str(e), "receivers": []}
        saved = _load()
        try:
            sources = _call("Sources")["data"][0]
        except Exception:
            sources = ["fast", "monitor"]
        try:
            local_audio = _busctl("get-property", BUS, PATH, IFACE, "LocalAudio")["data"]
        except Exception:
            local_audio = True
        receivers = []
        for rid, name, model, addr, _iface, _port, connected, mirroring in rows:
            choice = saved.get(rid, {})
            receivers.append({
                "id": rid, "name": name, "model": model, "address": addr,
                "connected": connected, "mirroring": mirroring,
                "source": choice.get("source", sources[0] if sources else "fast"),
                "size": choice.get("size", "1920x1080"),
            })
        return {"up": True, "receivers": receivers, "sources": sources, "sizes": SIZES, "local_audio": local_audio}

    async def choose(self, rid: str, source: str, size: str):
        data = _load()
        data[rid] = {"source": source, "size": size}
        _save(data)

    async def start(self, rid: str, source: str, size: str):
        w, h = size.split("x")
        await self.choose(rid, source, size)
        try:
            _call("MirrorSized", "ssuu", rid, source, w, h)
        except RuntimeError:  # an older luchd without MirrorSized: 1920x1080
            _call("Mirror", "ss", rid, source)

    async def stop(self, rid: str):
        _call("StopMirror", "s", rid)

    async def set_local_audio(self, on: bool):
        _busctl("set-property", BUS, PATH, IFACE, "LocalAudio", "b", "true" if on else "false")

    async def _main(self):
        """Start the fast-mode grabber: a privileged luch-kms that only this user can talk to."""
        global _helper
        exe = os.path.join(HELPER_DIR, "luch-kms")
        if os.path.isfile(exe):
            env = dict(os.environ, LD_LIBRARY_PATH=HELPER_DIR)
            _helper = subprocess.Popen([exe, "--serve", SOCKET, "--uid", str(UID)], env=env)
            decky.logger.info(f"Luch helper started (pid {_helper.pid})")
        else:
            decky.logger.info("Luch helper not found: fast mode off")

    async def _unload(self):
        global _helper
        if _helper:
            _helper.terminate()
            _helper = None
        decky.logger.info("Luch plugin unloaded")
