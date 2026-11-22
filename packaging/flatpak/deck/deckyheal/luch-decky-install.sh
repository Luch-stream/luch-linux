#!/bin/bash
# Installs or updates Decky Loader from the first source that answers: the ALS mirror, then GitHub, or the other way
# round (LUCH_DECKY_ORDER="github als"). Run as root; LUCH_USER is the Deck user.
#   luch-decky-install.sh latest    print the latest release tag from the first source that answers
#   luch-decky-install.sh           install it (PluginLoader, version file, systemd unit) and start the loader
set -u
ORDER=${LUCH_DECKY_ORDER:-"als github"}
ALS=${LUCH_DECKY_ALS:-https://altlinux.space/plafonlinux/decky-loader}
GH=https://github.com/SteamDeckHomebrew/decky-loader
case "$(uname -m)" in aarch64 | arm64) ASSET=PluginLoader-arm64 ;; *) ASSET=PluginLoader ;; esac
say() { echo "luch-decky: $*" >&2; }

# release <source> -> "tag url", from the release list of that source
release() {
    local api
    case $1 in
        als) api="${ALS/\/\/altlinux.space\//\/\/altlinux.space\/api\/v1\/repos\/}/releases?limit=10" ;;
        github) api=https://api.github.com/repos/SteamDeckHomebrew/decky-loader/releases ;;
    esac
    local j; j=$(curl -fsSL -m 15 "$api") || return 1
    python3 -c '
import json, sys
for r in json.load(sys.stdin):
    if r.get("prerelease") or r.get("draft"): continue
    for a in r.get("assets", []):
        if a["name"] == sys.argv[1]:
            print(r["tag_name"], a["browser_download_url"]); sys.exit(0)
sys.exit(1)' "$ASSET" <<< "$j"
}

# unit <source> -> the release unit file of Decky, from that source
unit_url() { case $1 in als) echo "$ALS/raw/branch/main/dist/plugin_loader-release.service" ;;
                        github) echo "https://raw.githubusercontent.com/SteamDeckHomebrew/decky-loader/main/dist/plugin_loader-release.service" ;; esac; }

for src in $ORDER; do
    rel=$(release "$src") || { say "$src: no release found, trying the next source"; continue; }
    tag=${rel%% *}; url=${rel#* }
    [ "${1:-}" = latest ] && { echo "$tag"; exit 0; }

    HOME_DIR=$(getent passwd "$LUCH_USER" | cut -d: -f6); HB=$HOME_DIR/homebrew
    tmp=$(mktemp -d)
    say "$src: downloading Decky Loader $tag"
    curl -fL -m 300 -o "$tmp/PluginLoader" "$url" || { say "$src: download failed, trying the next source"; rm -rf "$tmp"; continue; }
    curl -fsSL -m 30 -o "$tmp/unit" "$(unit_url "$src")" ||
        cp "$HB/services/.systemd/plugin_loader-release.service" "$tmp/unit" 2>/dev/null ||
        { say "$src: no unit file, trying the next source"; rm -rf "$tmp"; continue; }

    systemctl stop plugin_loader 2>/dev/null
    install -d -o "$LUCH_USER" "$HB" "$HB/plugins"
    rm -rf "$HB/services"; install -d -o "$LUCH_USER" "$HB/services" "$HB/services/.systemd"
    install -m755 "$tmp/PluginLoader" "$HB/services/PluginLoader"
    echo "$tag" > "$HB/services/.loader.version"
    sed "s|\${HOMEBREW_FOLDER}|$HB|" "$tmp/unit" > /etc/systemd/system/plugin_loader.service
    cp "$tmp/unit" "$HB/services/.systemd/plugin_loader-release.service"
    touch "$HOME_DIR/.steam/steam/.cef-enable-remote-debugging" && chown "$LUCH_USER" "$HOME_DIR/.steam/steam/.cef-enable-remote-debugging"
    rm -rf "$tmp"
    systemctl daemon-reload; systemctl enable plugin_loader; systemctl start plugin_loader
    say "Decky Loader $tag installed from $src"; exit 0
done
say "no source answered (tried: $ORDER)"; exit 1
