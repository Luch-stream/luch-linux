# Luch Flatpak and the Steam Deck setup

`toxblh.luch.Luch.yml` builds `luchd`, `luchctl` and `luch-kms` with a minimal FFmpeg (VA-API encoders only).
Besides the daemon it carries what a Steam Deck needs, so one file is the whole install:

- `deck/luch-setup`: puts the Decky Loader plugin (and Decky Loader itself, if it is missing, after a
  confirmation) and a user service for `luchd` on the host. What needs root runs through `pkexec`.
  `luch-setup --list` / `--dry-run` / `--remove`.
- `deck/decky-plugin`: the built plugin from `../../decky` (`pnpm build`, copy `dist/index.js`).
- the fast-mode helper (`luch-kms` and the two FFmpeg libraries it needs) under `/app/share/luch/helper`:
  Decky starts it as root, `luchd` in the sandbox talks to it through `$XDG_RUNTIME_DIR/luch/kms.sock`.

Build (crates are vendored offline):

    cargo vendor --locked packaging/flatpak/vendor      # at the repo root
    cd packaging/flatpak
    flatpak-builder --user --install-deps-from=flathub --force-clean --repo=repo build toxblh.luch.Luch.yml
    flatpak build-bundle repo luch.flatpak toxblh.luch.Luch

## Steam Deck: how a user installs it

1. On the Deck, in Desktop Mode, open the Luch site and download **Install Luch** (`deck/Install-Luch.desktop`, a shortcut).
2. Double-click it: it runs `deck/install.sh`, which downloads `luch.flatpak` from ALS (GitHub if ALS does not answer),
   checks its checksum, installs the Flatpak (`toxblh.luch.Luch`) and starts `luch-setup`.
3. `luch-setup` asks for the password once and installs (a window with the progress): Decky Loader (installed or updated, from ALS,
   then GitHub), the Luch plugin, `luch-deckyheal` (a root timer that puts the plugin back and restarts or reinstalls Decky Loader if a
   SteamOS update broke them), the Luch widget for the system tray (`deck/plasmoid`), and `luchd` as a service of the session.
4. Open the widget in the tray (Desktop Mode) or the Decky tab in Game Mode.

`luch-setup --remove` takes it all away. Release files (`luch.flatpak`, `luch.flatpak.sha256`, `install.sh`) are published
as the `deck-latest` release.
