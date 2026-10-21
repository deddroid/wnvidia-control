#!/bin/sh
# Install wnvctl for the current user in ~/.local, and optionally the root
# service (--service, uses sudo).
#
#   ./install.sh             GUI, CLI, desktop entry and icons in ~/.local
#   ./install.sh --service   also the daemon in /usr/local/bin + systemd unit
#   ./install.sh --uninstall
set -eu

here=$(cd "$(dirname "$0")" && pwd)
prefix="${PREFIX:-$HOME/.local}"
unit=/etc/systemd/system/wnvctl.service
service=0
uninstall=0
for arg in "$@"; do
    case "$arg" in
        --service) service=1 ;;
        --uninstall) uninstall=1 ;;
        -h|--help) sed -n '2,8p' "$0"; exit 0 ;;
        *) echo "unknown option: $arg" >&2; exit 2 ;;
    esac
done

# Files may come from a release archive (flat) or a source checkout.
find_file() {
    for p in "$@"; do [ -e "$here/$p" ] && { echo "$here/$p"; return; }; done
    return 1
}

if [ "$uninstall" = 1 ]; then
    rm -f "$prefix/bin/wnvctl" "$prefix/bin/wnvctl-gui" \
          "$prefix/share/applications/wnvctl.desktop" \
          "$HOME/.config/autostart/wnvctl.desktop"
    for s in 16 32 48 64 128 256 512; do
        rm -f "$prefix/share/icons/hicolor/${s}x${s}/apps/wnvctl.png"
    done
    rm -f "$prefix/share/icons/hicolor/scalable/apps/wnvctl.svg"
    if [ -e "$unit" ]; then
        echo "Removing the service (sudo)"
        sudo systemctl disable --now wnvctl || true
        sudo rm -f "$unit" /usr/local/bin/wnvctl /usr/lib/sysusers.d/wnvctl.conf
        sudo systemctl daemon-reload
    fi
    echo "Uninstalled. Settings are kept in ${XDG_CONFIG_HOME:-$HOME/.config}/wnvctl."
    exit 0
fi

cli=$(find_file wnvctl target/release/wnvctl) || { echo "wnvctl binary not found (run cargo build --release)" >&2; exit 1; }
gui=$(find_file wnvctl-gui target/release/wnvctl-gui) || gui=""
install -Dm755 "$cli" "$prefix/bin/wnvctl"
if [ -n "$gui" ]; then
    install -Dm755 "$gui" "$prefix/bin/wnvctl-gui"
    desktop=$(find_file wnvctl.desktop packaging/linux/wnvctl.desktop)
    install -Dm644 "$desktop" "$prefix/share/applications/wnvctl.desktop"
    for s in 16 32 48 64 128 256 512; do
        icon=$(find_file "icons/icon-$s.png" "assets/icon-$s.png") && \
            install -Dm644 "$icon" "$prefix/share/icons/hicolor/${s}x${s}/apps/wnvctl.png"
    done
    svg=$(find_file icons/icon.svg assets/icon.svg) && \
        install -Dm644 "$svg" "$prefix/share/icons/hicolor/scalable/apps/wnvctl.svg"
    command -v update-desktop-database >/dev/null && update-desktop-database "$prefix/share/applications" 2>/dev/null || true
    command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q "$prefix/share/icons/hicolor" 2>/dev/null || true
fi

if [ "$service" = 1 ]; then
    src=$(find_file wnvctl.service packaging/linux/wnvctl.service)
    sysusers=$(find_file wnvctl.sysusers packaging/linux/wnvctl.sysusers)
    echo "Installing the service (sudo)"
    # The daemon runs as root: it must not live in a user-writable folder.
    sudo install -Dm755 "$cli" /usr/local/bin/wnvctl
    sed 's|/usr/bin/wnvctl|/usr/local/bin/wnvctl|' "$src" | sudo tee "$unit" >/dev/null
    sudo install -Dm644 "$sysusers" /usr/lib/sysusers.d/wnvctl.conf
    sudo systemd-sysusers
    sudo systemctl daemon-reload
    sudo systemctl enable --now wnvctl
else
    echo "Note: to change GPU settings install the service: $0 --service"
fi
echo "Installed in $prefix. Make sure $prefix/bin is in your PATH."
