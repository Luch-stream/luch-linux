# Luch Flatpak and the Steam Deck setup

`dev.luch.Luch.yml` builds `luchd`, `luchctl` and `luch-kms` with a minimal FFmpeg (VA-API encoders only).
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
    flatpak-builder --user --install-deps-from=flathub --force-clean --repo=repo build dev.luch.Luch.yml
    flatpak build-bundle repo luch.flatpak dev.luch.Luch
