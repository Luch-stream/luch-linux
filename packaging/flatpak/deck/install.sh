#!/bin/bash
# Luch for the Steam Deck, step one: download the Luch Flatpak and start its installer (luch-setup).
# This is what the "Install Luch" shortcut from the site runs. Sources are tried in turn: ALS first, GitHub if ALS
# does not answer (LUCH_SOURCES="github als" turns that round).
set -u
APP=toxblh.luch.Luch
SOURCES=${LUCH_SOURCES:-"als github"}
URL_als=https://altlinux.space/plafonlinux/luch-linux/releases/download/deck-latest
URL_github=https://github.com/plafonlinux/luch-linux/releases/download/deck-latest

case "${LC_ALL:-${LC_MESSAGES:-${LANG:-}}}" in ru*) RU=1 ;; *) RU=0 ;; esac
t() { if [ "$RU" = 1 ]; then echo "$2"; else echo "$1"; fi; }
kd() { command -v kdialog >/dev/null && kdialog --title "$(t Luch Луч)" "$@"; }
die() { kd --error "$1" || echo "$1" >&2; exit 1; }

command -v flatpak >/dev/null || die "$(t 'Flatpak is not installed on this system.' 'В системе нет Flatpak.')"
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT

ref=$(kd --progressbar "$(t 'Downloading the Luch installer...' 'Скачиваем установщик Луча...')" 0)   # 0: a busy bar
close() { [ -n "$ref" ] && qdbus6 $ref close >/dev/null 2>&1 || true; }

got=
for src in $SOURCES; do
    base=URL_$src; base=${!base}
    if curl -fsSL -m 120 -o "$tmp/luch.flatpak" "$base/luch.flatpak" && curl -fsSL -m 20 -o "$tmp/luch.flatpak.sha256" "$base/luch.flatpak.sha256" &&
        (cd "$tmp" && sha256sum -c --status luch.flatpak.sha256); then got=$src; break; fi
    echo "luch-install: $src did not give a good file, trying the next source" >&2
done
[ -n "$got" ] || { close; die "$(t 'Could not download the installer (tried ALS and GitHub). Check the network and try again.' 'Не удалось скачать установщик (пробовали ALS и GitHub). Проверьте сеть и повторите.')"; }

flatpak install --user --noninteractive -y --or-update "$tmp/luch.flatpak" >"$tmp/flatpak.log" 2>&1 ||
    { close; die "$(t 'The Flatpak did not install:' 'Flatpak не установился:')"$'\n'"$(tail -n 5 "$tmp/flatpak.log")"; }
close
exec flatpak run --command=luch-setup "$APP"
