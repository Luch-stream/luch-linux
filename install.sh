#!/bin/sh
# Builds and installs Luch for the current user: binaries, luch-kms capability, user service,
# GNOME extension, "Show on TV" in the file managers. SPDX-License-Identifier: GPL-3.0-or-later
set -eu
cd "$(dirname "$0")"

for crate in luchd luchctl luch-kms; do
    cargo install --locked --path "$crate"
done
BIN="${CARGO_HOME:-$HOME/.cargo}/bin"

echo "Granting CAP_SYS_ADMIN to $BIN/luch-kms (needed for the 'fast' KMS mode; sudo asks for a password)"
sudo setcap cap_sys_admin+ep "$BIN/luch-kms" || echo "setcap failed: 'fast' mode won't work, portal modes will"

install -Dm644 contrib/luchd.service "$HOME/.config/systemd/user/luchd.service"
sed -i "s|%h/.cargo/bin/luchd|$BIN/luchd|" "$HOME/.config/systemd/user/luchd.service"
systemctl --user daemon-reload
systemctl --user enable luchd.service
systemctl --user restart luchd.service

EXT="$HOME/.local/share/gnome-shell/extensions/luch@toxblh.github.io"
if command -v gnome-shell >/dev/null 2>&1; then
    rm -rf "$EXT"
    cp -r gnome-extension/luch@toxblh.github.io "$EXT"
    gnome-extensions enable luch@toxblh.github.io 2>/dev/null ||
        echo "Log out and back in, then: gnome-extensions enable luch@toxblh.github.io"
fi

# "Show on TV" from the file managers: Open With in any of them, the menu of GNOME Files (with nautilus-python) and
# of Dolphin. They run luchctl by its full path: a desktop session's PATH often lacks ~/.cargo/bin.
FM=contrib/file-managers
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
put() { # file, where, mode
    install -Dm"$3" "$1" "$2"
    sed -i "s|^Exec=luchctl |Exec=$BIN/luchctl |" "$2"
}
install -Dm644 assets/luch.svg "$DATA/icons/hicolor/scalable/apps/luch.svg"
for f in "$FM"/applications/*.desktop; do put "$f" "$DATA/applications/${f##*/}" 644; done
update-desktop-database "$DATA/applications" 2>/dev/null || :
if command -v nautilus >/dev/null 2>&1; then
    install -Dm644 "$FM/nautilus/luch.py" "$DATA/nautilus-python/extensions/luch.py"
    echo "GNOME Files: 'Show on TV' in the menu once nautilus-python is installed and Files restarted (nautilus -q)"
fi
if command -v dolphin >/dev/null 2>&1; then
    # Plasma 6 reads kio/servicemenus (and wants them executable), Plasma 5 kservices5/ServiceMenus.
    for dir in kio/servicemenus kservices5/ServiceMenus; do
        for f in "$FM"/dolphin/*.desktop; do put "$f" "$DATA/$dir/${f##*/}" 755; done
    done
fi

echo "Done. Try: luchctl"
