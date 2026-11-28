# The menu GNOME Files gets, without GNOME: python3 -m unittest discover -s contrib/file-managers/nautilus
# SPDX-License-Identifier: GPL-3.0-or-later

import importlib
import sys
import types
import unittest

sys.modules["gi"] = None  # as on a desktop without nautilus-python: only the menu's logic is loaded
import luch  # noqa: E402

TVS = [("id-1", "Living room TV"), ("id-2", "Kitchen")]


class Menu(unittest.TestCase):
    def test_one_tv_is_one_click(self):
        items = luch.menu([("/p/a.jpg", "image/jpeg"), ("/p/b.png", "image/png")], TVS[:1], "luchctl")
        self.assertEqual(items, [{"name": "Luch::show", "label": "Show on Living room TV",
                                  "argv": ["luchctl", "--pick", "cast", "id-1", "/p/a.jpg", "/p/b.png"]}])

    def test_several_tvs_are_a_submenu(self):
        [show] = luch.menu([("/p/film.mkv", "video/x-matroska")], TVS, "luchctl")
        self.assertEqual(show["label"], "Show on TV")
        self.assertEqual([c["label"] for c in show["children"]], ["Living room TV", "Kitchen"])
        self.assertEqual(show["children"][1]["argv"], ["luchctl", "--pick", "cast", "id-2", "/p/film.mkv"])
        self.assertEqual(show["children"][1]["name"], "Luch::show::id-2")

    def test_slides_can_also_be_presented(self):
        show, present = luch.menu([("/p/talk.pptx", "application/vnd.ms-powerpoint")], TVS, "luchctl")
        self.assertEqual(present["label"], "Present on TV")
        self.assertEqual(present["children"][0]["label"], "Living room TV…")
        self.assertEqual(present["children"][0]["argv"], ["luchctl", "--pick", "present", "id-1", "/p/talk.pptx"])
        [_, one] = luch.menu([("/p/deck.PDF", "application/pdf")], TVS[:1], "luchctl")
        self.assertEqual(one["label"], "Present on Living room TV…")

    def test_no_tv_yet_lets_luchctl_say_so(self):
        [show] = luch.menu([("/p/song.flac", "audio/flac")], [], "luchctl")
        self.assertEqual(show, {"name": "Luch::show", "label": "Show on TV with Luch",
                                "argv": ["luchctl", "--pick", "cast", "/p/song.flac"]})

    def test_what_is_not_for_a_tv(self):
        self.assertEqual(luch.menu([("/p/a.jpg", "image/jpeg"), ("/p/notes.zip", "application/zip")], TVS), [])
        self.assertEqual(luch.menu([], TVS), [])
        self.assertEqual(luch.menu([(None, "image/jpeg")], TVS), [])
        # Several slides are shown, not presented: present takes one.
        items = luch.menu([("/p/a.pdf", "application/pdf"), ("/p/b.pdf", "application/pdf")], TVS)
        self.assertEqual([i["name"] for i in items], ["Luch::show"])


class FakeGI:
    """Just enough of gi.repository for the extension's class: Files' items, and a bus that answers Receivers."""

    def __init__(self, rows):
        self.subscribed = []
        fake = self

        class Item:
            def __init__(self, name, label):
                self.name, self.label, self.submenu, self.activate = name, label, None, None

            def set_submenu(self, m):
                self.submenu = m

            def connect(self, _signal, f):
                self.activate = lambda: f(self)

        class Menu(list):
            append_item = list.append

        class Bus:
            def signal_subscribe(self, *a):
                fake.subscribed.append(a[2])

            def call(self, *a):
                a[-1](self, None)

            def call_finish(self, _):
                return types.SimpleNamespace(unpack=lambda: (rows,))

        ns = types.SimpleNamespace
        self.repo = ns(
            Nautilus=ns(MenuProvider=type("MenuProvider", (), {}), MenuItem=Item, Menu=Menu),
            GObject=ns(GObject=type("GObject", (), {})),
            GLib=ns(Error=Exception, VariantType=str),
            Gio=ns(bus_get_sync=lambda *_: Bus(), BusType=ns(SESSION=0), DBusSignalFlags=ns(NONE=0),
                   BusNameWatcherFlags=ns(NONE=0), DBusCallFlags=ns(NO_AUTO_START=0),
                   bus_watch_name_on_connection=lambda bus, name, flags, up, down: up(bus, name, ":1.5")))

    def load(self):
        gi = types.ModuleType("gi")
        gi.require_version = lambda *_: None
        repo = types.ModuleType("gi.repository")
        for k, v in vars(self.repo).items():
            setattr(repo, k, v)
        saved = {k: sys.modules.get(k) for k in ("gi", "gi.repository", "luch")}
        sys.modules.update({"gi": gi, "gi.repository": repo})
        sys.modules.pop("luch", None)
        try:
            return importlib.import_module("luch")
        finally:
            for k, v in saved.items():
                sys.modules[k] = v


class File:
    def __init__(self, path, mime, directory=False):
        self.path, self.mime, self.directory = path, mime, directory

    def get_location(self):
        return types.SimpleNamespace(get_path=lambda: self.path)

    def get_mime_type(self):
        return self.mime

    def is_directory(self):
        return self.directory


class Extension(unittest.TestCase):
    def test_files_menu_from_luchds_receivers(self):
        fake = FakeGI([("id-1", "Living room TV", "m", "addr", "eth0", 47800, False, False),
                       ("id-2", "Kitchen", "m", "addr", "eth0", 47800, False, False)])
        module = fake.load()
        ext = module.LuchMenu()
        self.assertEqual(fake.subscribed, ["ReceiversChanged"])
        launched = []
        module.launch = launched.append
        items = ext.get_file_items([File("/p/a.pdf", "application/pdf")])  # Nautilus 43+
        self.assertEqual([i.label for i in items], ["Show on TV", "Present on TV"])
        self.assertEqual([c.label for c in items[1].submenu], ["Living room TV…", "Kitchen…"])
        items[1].submenu[1].activate()
        self.assertEqual(launched[0][1:], ["--pick", "present", "id-2", "/p/a.pdf"])
        self.assertEqual(ext.get_file_items(None, [File("/p", "inode/directory", True)]), [])  # older Nautilus


if __name__ == "__main__":
    unittest.main()
