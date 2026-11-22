"""Backend of the Luch plugin: talks to luchd (session bus, dev.luch.Luch1) with busctl.

Decky runs plugins as the user, but the session bus address is not always in the environment,
so it is built from the user's runtime directory.
"""
import asyncio
import filecmp
import glob
import hashlib
import json
import os
import pwd
import shutil
import subprocess
import tempfile
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

# Luch itself (luchd, luch-kms) is a Flatpak. A plugin from the Decky store brings only this panel, so the panel can fetch
# and set up the rest itself, from the same release the installer on the Luch site uses (ALS first, GitHub after it).
APP_ID = "toxblh.luch.Luch"
RELEASES = [
    "https://altlinux.space/plafonlinux/luch-linux/releases/download/deck-latest",
    "https://github.com/plafonlinux/luch-linux/releases/download/deck-latest",
]
HOME = pwd.getpwnam(USER).pw_dir
UNIT_FILE = os.path.join(HOME, ".config/systemd/user/luchd-flatpak.service")
UNIT = """[Unit]
Description=Luch sender daemon (Flatpak)

[Service]
ExecStartPre=-/usr/bin/pkill -x luchd
ExecStart=/usr/bin/flatpak run toxblh.luch.Luch
ExecStop=-/usr/bin/pkill -x luchd
Restart=on-failure

[Install]
WantedBy=default.target
"""
INSTALL_STAGES = 4
_inst = {"running": False, "done": False, "stage": 0, "text": "", "error": ""}

# The built-in screen goes off this long after the last Steam menu closed, while a mirror runs.
DIM_AFTER = 4.0
CEF = "http://127.0.0.1:8080"
MENU_WINDOWS = ("QuickAccess", "MainMenu")   # the "···" panel and the Steam button menu


def _backlight():
    dirs = sorted(glob.glob("/sys/class/backlight/*/"))
    return dirs[0] if dirs else None


def _dim(dark: bool):
    """Brightness to the minimum, or back to what it was. The display output stays active, so the picture is still
    captured. The old brightness is kept on disk: a crash must not leave the screen dark (_main puts it back)."""
    bl = _backlight()
    if not bl:
        return
    data = _load()
    try:
        if dark and "saved_brightness" not in data:
            with open(bl + "brightness") as f:
                data["saved_brightness"] = int(f.read())
            _save(data)
            with open(bl + "brightness", "w") as f:
                f.write("0")
        elif not dark:
            if _read(bl + "bl_power") != "0":   # an earlier version switched the backlight power off
                with open(bl + "bl_power", "w") as f:
                    f.write("0")
            if "saved_brightness" in data:
                with open(bl + "brightness", "w") as f:
                    f.write(str(data.pop("saved_brightness")))
                _save(data)
    except OSError as e:
        decky.logger.error(f"backlight: {e}")


def _read(path):
    try:
        with open(path) as f:
            return f.read().strip()
    except OSError:
        return ""


def _as_user(*args, timeout=60):
    cmd = ["runuser", "-u", USER, "--", "env", f"XDG_RUNTIME_DIR={RUNTIME}",
           f"DBUS_SESSION_BUS_ADDRESS=unix:path={RUNTIME}/bus", *args]
    return subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)


def _installed():
    return os.path.isdir(os.path.join(HOME, ".local/share/flatpak/app", APP_ID))


def _location():
    r = _as_user("flatpak", "info", "--user", "--show-location", APP_ID, timeout=20)
    return r.stdout.strip() if r.returncode == 0 else ""


def _ensure_helper():
    """The fast-mode helper in this plugin's folder is the one that ships with the installed Flatpak. A plugin update
    from the store replaces the folder, and a Flatpak update brings a new helper: either way it is copied again."""
    loc = _location()
    src = os.path.join(loc, "files/share/luch/helper") if loc else ""
    if not os.path.isfile(os.path.join(src, "luch-kms")):
        return False
    dst = os.path.join(HELPER_DIR, "luch-kms")
    if os.path.isfile(dst) and filecmp.cmp(os.path.join(src, "luch-kms"), dst, shallow=False):
        return False
    _kill_stale_helpers()
    os.makedirs(HELPER_DIR, exist_ok=True)
    shutil.copytree(src, HELPER_DIR, dirs_exist_ok=True)
    os.chmod(dst, 0o755)
    decky.logger.info("Luch helper copied from the Flatpak")
    return True


def _start_helper():
    global _helper
    exe = os.path.join(HELPER_DIR, "luch-kms")
    if _helper and _helper.poll() is None:
        return
    if os.path.isfile(exe):
        env = dict(os.environ, LD_LIBRARY_PATH=HELPER_DIR)
        _helper = subprocess.Popen([exe, "--serve", SOCKET, "--uid", str(UID)], env=env)
        decky.logger.info(f"Luch helper started (pid {_helper.pid})")
    else:
        decky.logger.info("Luch helper not found: fast mode off")


async def _download():
    """luch.flatpak from the first source that gives a file with the right hash; returns its path."""
    work = tempfile.mkdtemp(prefix="luch-install-", dir="/var/tmp")
    os.chmod(work, 0o755)
    async with aiohttp.ClientSession() as http:
        for base in RELEASES:
            try:
                async with http.get(f"{base}/luch.flatpak.sha256", timeout=aiohttp.ClientTimeout(total=20)) as r:
                    r.raise_for_status()
                    want = (await r.text()).split()[0]
                path = os.path.join(work, "luch.flatpak")
                h = hashlib.sha256()
                async with http.get(f"{base}/luch.flatpak", timeout=aiohttp.ClientTimeout(total=300)) as r:
                    r.raise_for_status()
                    total, got = int(r.headers.get("Content-Length", 0)), 0
                    with open(path, "wb") as f:
                        async for chunk in r.content.iter_chunked(1 << 16):
                            f.write(chunk)
                            h.update(chunk)
                            got += len(chunk)
                            _inst["text"] = f"{got * 100 // total} %" if total else ""
                if h.hexdigest() == want:
                    os.chmod(path, 0o644)
                    return path
                decky.logger.warning(f"{base}: the hash does not match")
            except Exception as e:
                decky.logger.warning(f"{base}: {e}")
    raise RuntimeError("download")


def _flatpak_install(path):
    r = _as_user("flatpak", "install", "--user", "--noninteractive", "-y", "--or-update", path, timeout=1200)
    if r.returncode != 0:
        raise RuntimeError("flatpak: " + (r.stderr or r.stdout).strip().splitlines()[-1][:200])


def _enable_service():
    os.makedirs(os.path.dirname(UNIT_FILE), exist_ok=True)
    with open(UNIT_FILE, "w") as f:
        f.write(UNIT)
    for d in (os.path.dirname(UNIT_FILE), os.path.dirname(os.path.dirname(UNIT_FILE))):
        shutil.chown(d, USER, USER)
    shutil.chown(UNIT_FILE, USER, USER)
    _as_user("systemctl", "--user", "daemon-reload")
    r = _as_user("systemctl", "--user", "enable", "--now", "luchd-flatpak.service")
    if r.returncode != 0:
        raise RuntimeError("service: " + r.stderr.strip()[:200])
    # SteamOS ships Avahi with publishing off, and luchd then cannot announce this Deck as a sender.
    conf = "/etc/avahi/avahi-daemon.conf"
    try:
        text = open(conf).read()
        fixed = text.replace("disable-publishing=yes", "disable-publishing=no").replace(
            "disable-user-service-publishing=yes", "disable-user-service-publishing=no")
        if fixed != text:
            open(conf, "w").write(fixed)
            subprocess.run(["systemctl", "restart", "avahi-daemon"], timeout=30)
            _as_user("systemctl", "--user", "restart", "luchd-flatpak.service")
    except OSError as e:
        decky.logger.warning(f"avahi: {e}")


async def _run_install():
    _inst.update(running=True, done=False, stage=1, text="", error="")
    try:
        path = await _download()
        _inst.update(stage=2, text="")
        await asyncio.to_thread(_flatpak_install, path)
        shutil.rmtree(os.path.dirname(path), ignore_errors=True)
        _inst.update(stage=3)
        await asyncio.to_thread(_enable_service)
        await asyncio.to_thread(_ensure_helper)
        _start_helper()
        _inst.update(stage=4)
        for _ in range(30):
            try:
                await asyncio.to_thread(_call, "Receivers")
                break
            except Exception:
                await asyncio.sleep(1)
        else:
            raise RuntimeError("luchd does not answer")
        _inst.update(done=True)
    except Exception as e:
        decky.logger.error(f"install: {e}")
        _inst["error"] = str(e)
    finally:
        _inst["running"] = False


def _kill_stale_helpers():
    """Decky restarts leave the previous run's helper alive (KillMode=process): one helper is enough.
    Returns how many were stopped: the streams they carried are gone with them."""
    n = 0
    exe = os.path.join(HELPER_DIR, "luch-kms")
    for pid in filter(str.isdigit, os.listdir("/proc")):
        try:
            with open(f"/proc/{pid}/cmdline", "rb") as f:
                argv = f.read().split(b"\0")
            if argv[0].decode() == exe and b"--serve" in argv:
                os.kill(int(pid), 15)
                n += 1
        except (OSError, ValueError):
            pass
    return n


def _stop_all():
    """End every running session, so the panel does not claim a mirror that no longer exists."""
    try:
        for row in _call("Receivers")["data"][0]:
            if row[7]:
                _call("StopMirror", "s", row[0])
    except Exception as e:
        decky.logger.info(f"stop sessions: {e}")


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


def _codec(rid, choice):
    """The codec luchd remembers for a receiver; this plugin's own copy when luchd cannot say."""
    try:
        return _call("GetCodec", "s", rid)["data"][0]
    except RuntimeError:
        return choice.get("codec", "auto")


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
            return {"up": False, "error": str(e), "receivers": [], "installed": _installed(), "install": dict(_inst)}
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
                "codec": _codec(rid, choice),
            })
        return {"up": True, "receivers": receivers, "sources": sources, "sizes": SIZES, "local_audio": local_audio,
                "dim_screen": saved.get("dim_screen", False)}

    async def install_luch(self):
        """Fetch Luch (a Flatpak), set the service up and start it: what the installer on the site does, from the panel."""
        if not _inst["running"]:
            asyncio.create_task(_run_install())

    async def install_status(self):
        return dict(_inst)

    async def choose(self, rid: str, source: str, size: str):
        data = _load()
        data[rid] = {**data.get(rid, {}), "source": source, "size": size}
        _save(data)

    async def choose_codec(self, rid: str, codec: str):
        """auto, hevc or avc for this receiver: kept by luchd (so it holds for every front-end) and here as a fallback."""
        data = _load()
        data[rid] = {**data.get(rid, {}), "codec": codec}
        _save(data)
        try:
            _call("SetCodec", "ss", rid, codec)
        except RuntimeError:  # an older luchd without SetCodec
            pass

    async def start(self, rid: str, source: str, size: str):
        w, h = size.split("x")
        await self.choose(rid, source, size)
        try:  # the saved choice of the video codec (an older luchd has no SetCodec: it picks by itself)
            _call("SetCodec", "ss", rid, _load().get(rid, {}).get("codec", "auto"))
        except RuntimeError:
            pass
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
        global _dimmer_task
        _dim(False)   # a screen left dark by a crash comes back
        _dimmer_task = asyncio.create_task(_dim_loop())
        if _kill_stale_helpers():
            await asyncio.to_thread(_stop_all)
        if _installed():
            await asyncio.to_thread(_ensure_helper)
        _start_helper()

    async def _unload(self):
        global _helper, _dimmer_task
        if _dimmer_task:
            _dimmer_task.cancel()
            _dimmer_task = None
        _dim(False)
        if _helper:
            await asyncio.to_thread(_stop_all)
            _helper.terminate()
            _helper = None
        decky.logger.info("Luch plugin unloaded")
