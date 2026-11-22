#!/bin/bash
# Luch Decky self-heal, run as root by luch-deckyheal.timer (boot, then every 15 minutes). It puts back what a SteamOS or
# Steam update took away: Decky Loader itself (unit, binary, a loader that does not answer) and the Luch plugin.
# Everything it does goes to the journal: journalctl -u luch-deckyheal
set -u
. /etc/luch/heal.conf                      # LUCH_USER=<the Deck user>
HOME_DIR=$(getent passwd "$LUCH_USER" | cut -d: -f6)
HB=$HOME_DIR/homebrew
PLUG=$HB/plugins/Luch
BACKUP=/var/lib/luch/plugin
STAMP=/var/lib/luch/decky-reinstall
say() { echo "luch-deckyheal: $*"; }

# Healthy: the binary is there, the unit is active, and the loader answers on its port (it needs a few seconds after a start).
loader_ok() {
    local i
    [ -x "$HB/services/PluginLoader" ] || return 1
    for i in 1 2 3 4 5 6; do
        systemctl is-active --quiet plugin_loader && curl -s -m 3 -o /dev/null http://127.0.0.1:1337/ && return 0
        sleep 2
    done
    return 1
}

# 1. Decky's unit file vanished from /etc: Decky keeps a copy of it next to the binary.
UNIT=/etc/systemd/system/plugin_loader.service
if [ ! -f "$UNIT" ] && [ -f "$HB/services/.systemd/plugin_loader-release.service" ]; then
    say "plugin_loader.service was missing, restoring it"
    sed "s|\${HOMEBREW_FOLDER}|$HB|" "$HB/services/.systemd/plugin_loader-release.service" > "$UNIT"
    systemctl daemon-reload; systemctl enable --now plugin_loader
fi

# 2. The plugin: put it back from the copy luch-setup keeps, if a file it needs is gone.
need=0
for f in main.py dist/index.js plugin.json helper/luch-kms; do [ -e "$PLUG/$f" ] || need=1; done
if [ "$need" = 1 ] && [ -f "$BACKUP/main.py" ] && [ -d "$HB/plugins" ]; then
    say "the Luch plugin was damaged or removed, restoring it"
    rm -rf "$PLUG"; mkdir -p "$PLUG"; cp -a "$BACKUP/." "$PLUG/"
    systemctl restart plugin_loader
fi

# 3. Decky Loader does not run or does not answer: restart it, and if that does not help, reinstall it
#    (luch-decky-install.sh: the ALS mirror first, GitHub if ALS does not answer, at most once in 12 hours).
if ! loader_ok; then
    say "Decky Loader is not healthy, restarting it"
    systemctl restart plugin_loader 2>/dev/null; sleep 20
    if ! loader_ok; then
        last=$(cat "$STAMP" 2>/dev/null || echo 0)
        if [ $(( $(date +%s) - last )) -lt 43200 ]; then
            say "Decky Loader is still down; it was reinstalled less than 12 hours ago, waiting"
        else
            date +%s > "$STAMP"
            LUCH_USER=$LUCH_USER /var/lib/luch/luch-decky-install.sh && loader_ok && say "Decky Loader is back" ||
                say "Decky Loader is still not answering"
        fi
    fi
fi
