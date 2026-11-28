# GNOME Files (Nautilus): "Show on <TV>" and "Present on <TV>…" in the menu of photos, videos, music, PDFs and
# office files, with the receivers luchd sees now (one TV: one click; several: a submenu of their names). Needs
# nautilus-python (python3-nautilus, nautilus-python); install.sh puts this file in
# ~/.local/share/nautilus-python/extensions, and `nautilus -q` makes Files load it. Without it, "Open With → Show on
# TV" (dev.luch.Show.desktop) does the same, the receiver then asked in a dialog.
# SPDX-License-Identifier: GPL-3.0-or-later

import os
import shutil
import subprocess

try:
    import gi

    for version in ("4.0", "3.0"):
        try:
            gi.require_version("Nautilus", version)
            break
        except ValueError:
            pass
    from gi.repository import Gio, GLib, GObject, Nautilus
except ImportError:  # the tests, or a desktop without nautilus-python
    Gio = GLib = GObject = Nautilus = None

BUS = "dev.luch.Luch1"
PATH = "/dev/luch/Luch1"

# What luchd's cast takes besides photos, video and sound: PDFs and what LibreOffice turns into one.
DOCUMENTS = {"pdf", "odt", "ods", "odp", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "rtf", "key"}
# What `luchctl present` takes: slides with a presenter view.
SLIDES = {"pdf", "odp", "ppt", "pptx", "key"}


def extension(path):
    return os.path.splitext(path)[1][1:].lower()


def showable(path, mime):
    """A photo, a video, a sound or a document the receiver can be handed."""
    return mime.split("/")[0] in ("image", "video", "audio") or extension(path) in DOCUMENTS


def menu(files, receivers, luchctl="luchctl"):
    """The items for `files` [(path, mime)] with `receivers` [(id, name)]: a list of
    {"name", "label", "argv"} or {"name", "label", "children": [...]}; empty when they are not for a TV."""
    if not files or not all(path and showable(path, mime) for path, mime in files):
        return []
    paths = [path for path, _ in files]
    wanted = [("show", "cast", paths, "Show on {}", "")]
    if len(paths) == 1 and extension(paths[0]) in SLIDES:
        wanted.append(("present", "present", paths, "Present on {}…", "…"))
    items = []
    for key, verb, args, label, dots in wanted:
        name = "Luch::" + key
        if len(receivers) == 1:
            rid, rname = receivers[0]
            items.append({"name": name, "label": label.format(rname),
                          "argv": [luchctl, "--pick", verb, rid] + args})
        elif receivers:
            items.append({"name": name, "label": label.format("TV").rstrip("…"), "children": [
                {"name": "{}::{}".format(name, rid), "label": rname + dots,
                 "argv": [luchctl, "--pick", verb, rid] + args}
                for rid, rname in receivers]})
        else:
            # luchd not running or nothing found yet: luchctl says which, in a dialog.
            items.append({"name": name, "label": label.format("TV with Luch"),
                          "argv": [luchctl, "--pick", verb] + args})
    return items


def find_luchctl():
    """Files' PATH often lacks ~/.cargo/bin, where install.sh puts it."""
    home = os.path.expanduser("~")
    for p in (shutil.which("luchctl"), home + "/.cargo/bin/luchctl", home + "/.local/bin/luchctl"):
        if p and os.access(p, os.X_OK):
            return p
    return "luchctl"


def launch(argv):
    subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                     start_new_session=True)


class Receivers:
    """luchd's list, kept up to date by its ReceiversChanged, so opening a menu never waits for D-Bus."""

    def __init__(self):
        self.rows = []
        try:
            self.bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        except GLib.Error:
            self.bus = None
            return
        self.bus.signal_subscribe(BUS, BUS, "ReceiversChanged", PATH, None, Gio.DBusSignalFlags.NONE,
                                  lambda *_: self.refresh())
        # luchd started (or restarted) after Files.
        Gio.bus_watch_name_on_connection(self.bus, BUS, Gio.BusNameWatcherFlags.NONE,
                                         lambda *_: self.refresh(), lambda *_: self.clear())

    def clear(self):
        self.rows = []

    def refresh(self):
        self.bus.call(BUS, PATH, BUS, "Receivers", None, GLib.VariantType("(a(sssssqbb))"),
                      Gio.DBusCallFlags.NO_AUTO_START, 3000, None, self._got)

    def _got(self, bus, result):
        try:
            rows = bus.call_finish(result).unpack()[0]
        except GLib.Error:
            rows = []
        self.rows = [(r[0], r[1]) for r in rows]


if Nautilus is not None:
    class LuchMenu(GObject.GObject, Nautilus.MenuProvider):
        def __init__(self):
            super().__init__()
            self.receivers = Receivers()
            self.luchctl = find_luchctl()

        def get_file_items(self, *args):
            # Nautilus 43+ passes (files), older ones (window, files).
            files = []
            for f in args[-1]:
                location = f.get_location()
                path = location.get_path() if location else None
                if f.is_directory() or not path:
                    return []
                files.append((path, f.get_mime_type() or ""))
            return [self.item(i) for i in menu(files, self.receivers.rows, self.luchctl)]

        def item(self, i):
            item = Nautilus.MenuItem(name=i["name"], label=i["label"])
            if "children" in i:
                sub = Nautilus.Menu()
                item.set_submenu(sub)
                for c in i["children"]:
                    sub.append_item(self.item(c))
            else:
                item.connect("activate", lambda _item, argv=i["argv"]: launch(argv))
            return item
