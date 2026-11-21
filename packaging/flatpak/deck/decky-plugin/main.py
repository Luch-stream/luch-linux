"""Backend of the Luch plugin: talks to luchd (session bus, dev.luch.Luch1) with busctl.

Decky runs plugins as the user, but the session bus address is not always in the environment,
so it is built from the user's runtime directory.
"""
import asyncio
import glob
import json
import os
import pwd
import subprocess
import time

import aiohttp
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
_dimmer_task = None

# The built-in screen goes off this long after the last Steam menu closed, while a mirror runs.
DIM_AFTER = 4.0
CEF = "http://127.0.0.1:8080"
MENU_WINDOWS = ("QuickAccess", "MainMenu")   # the "···" panel and the Steam button menu


def _backlight():
    dirs = sorted(glob.glob("/sys/class/backlight/*/"))
    return dirs[0] if dirs else None


def _dim(dark: bool):
    """Backlight off (bl_power 4) or on. The display output stays active, so the picture is still captured.
    The brightness setting is not touched, so there is nothing to restore after a crash: _main turns it on."""
    bl = _backlight()
    if not bl:
        return
    want = "4" if dark else "0"
    try:
        with open(bl + "bl_power") as f:
            if f.read().strip() == want:
                return
        with open(bl + "bl_power", "w") as f:
            f.write(want)
    except OSError as e:
        decky.logger.error(f"backlight: {e}")


def _kill_stale_helpers():
    """Decky restarts leave the previous run's helper alive (KillMode=process): one helper is enough."""
    exe = os.path.join(HELPER_DIR, "luch-kms")
    for pid in filter(str.isdigit, os.listdir("/proc")):
        try:
            with open(f"/proc/{pid}/cmdline", "rb") as f:
                argv = f.read().split(b"\0")
            if argv[0].decode() == exe and b"--serve" in argv:
                os.kill(int(pid), 15)
        except (OSError, ValueError):
            pass


def _mirroring():
    return any(r[7] for r in _call("Receivers")["data"][0])


async def _menu_open(http):
    """Is the Steam button menu or the Quick Access panel on screen? Unknown counts as yes (screen stays on)."""
    try:
        async with http.get(f"{CEF}/json", timeout=aiohttp.ClientTimeout(total=2)) as r:
            tabs = await r.json()
        for t in tabs:
            if t.get("title", "").startswith(MENU_WINDOWS):
                async with http.ws_connect(t["webSocketDebuggerUrl"], timeout=2) as ws:
                    await ws.send_json({"id": 1, "method": "Runtime.evaluate",
                                        "params": {"expression": "document.visibilityState", "returnByValue": True}})
                    msg = await asyncio.wait_for(ws.receive_json(), 2)
                    if msg["result"]["result"]["value"] != "hidden":
                        return True
        return False
    except Exception:
        return True


async def _dim_loop():
    quiet = None
    mirroring, checked = False, 0.0
    async with aiohttp.ClientSession() as http:
        while True:
            enabled = _load().get("dim_screen", False)
            now = time.monotonic()
            try:
                if enabled and now - checked >= 1.5:   # busctl is the slow part, the menus are polled faster
                    mirroring, checked = await asyncio.to_thread(_mirroring), now
                if enabled and mirroring and not await _menu_open(http):
                    quiet = quiet or now
                    if now - quiet >= DIM_AFTER:
                        _dim(True)
                else:
                    quiet = None
                    _dim(False)
            except Exception as e:
                quiet, mirroring = None, False
                _dim(False)
                decky.logger.error(f"dimmer: {e}")
            await asyncio.sleep(0.4 if enabled and mirroring else 2 if not enabled else 1)


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
        return {"up": True, "receivers": receivers, "sources": sources, "sizes": SIZES, "local_audio": local_audio,
                "dim_screen": saved.get("dim_screen", False)}

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

    async def set_dim_screen(self, on: bool):
        data = _load()
        data["dim_screen"] = on
        _save(data)
        if not on:
            _dim(False)

    async def _main(self):
        """Start the fast-mode grabber (a privileged luch-kms that only this user can talk to) and the screen dimmer."""
        global _helper, _dimmer_task
        _dim(False)   # a screen left dark by a crash comes back
        _dimmer_task = asyncio.create_task(_dim_loop())
        exe = os.path.join(HELPER_DIR, "luch-kms")
        _kill_stale_helpers()
        if os.path.isfile(exe):
            env = dict(os.environ, LD_LIBRARY_PATH=HELPER_DIR)
            _helper = subprocess.Popen([exe, "--serve", SOCKET, "--uid", str(UID)], env=env)
            decky.logger.info(f"Luch helper started (pid {_helper.pid})")
        else:
            decky.logger.info("Luch helper not found: fast mode off")

    async def _unload(self):
        global _helper, _dimmer_task
        if _dimmer_task:
            _dimmer_task.cancel()
            _dimmer_task = None
        _dim(False)
        if _helper:
            _helper.terminate()
            _helper = None
        decky.logger.info("Luch plugin unloaded")
