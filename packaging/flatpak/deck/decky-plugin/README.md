# Decky Luch

A [Decky Loader](https://decky.xyz) plugin for [Luch](https://altlinux.space/toxblh/luch-linux): mirror the Steam Deck's
screen to a TV or projector from the Quick Access menu. Pick a receiver, a mode and a size; start and stop; choose
whether the Deck keeps playing sound.

It talks to the `luchd` daemon over the session bus (`dev.luch.Luch1`) with `busctl`, so `luchd` has to run: the Luch
Flatpak (`dev.luch.Luch`) or a native install.

Build: `pnpm install && pnpm build`, then copy the folder to `~/homebrew/plugins/Luch`.
