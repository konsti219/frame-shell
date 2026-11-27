#!/bin/bash
# Frame shell for the Steam Frame, with frame-monado. Run on the headset as steamos:
#   ./install.sh                     install or update from the bundle this script is in
#   install.sh uninstall [--purge]   remove it again (--purge: also settings, pairing and calibration)
#   install.sh monado DIR            replace Monado and its session files with DIR/monado and DIR/session (development)
# Everything lives in ~/.local/share/frame-shell, except the files listed in integrate() and unintegrate().
set -euo pipefail

prefix="$HOME/.local/share/frame-shell"
# The SteamOS version this release is built and tested for (the newest beta)
steamos_target=0.5.11
units="$HOME/.local/share/systemd/user"
marker="$XDG_RUNTIME_DIR/monado-stopped-on-purpose"
# frame-monado's units are in session/systemd, the shell's in share
monado_units=(monado.service monado-session.target monado-session-fallback.service)
# Settings, PC pairing and calibration, kept unless uninstall --purge
user_data=("$HOME/.config/frame-shell" "$HOME/.config/wayvr" "$HOME/.config/wivrn" "$HOME/.config/monado"
    "$HOME/.local/state/frame-shell" "$HOME/.local/state/frame-monado")

in_monado_session() { systemctl --user is-active --quiet monado-session.target; }

# Stops Monado (and the shell with it) without the fallback switching to SteamVR
stop_monado() {
    touch "$marker"
    systemctl --user stop frame-shell.service monado.service 2>/dev/null || true
}

# The files outside the prefix
integrate() {
    install -Dm644 -t "$units" "${monado_units[@]/#/$prefix/session/systemd/}" "$prefix/share/frame-shell.service"
    install -Dm644 -t "$units/monado-session-fallback.service.d" "$prefix/session/systemd/not-on-purpose.conf"
    local xdg="$units/steamos-manager.service.d/frame-monado.conf" manager_changed=false
    cmp -s "$prefix/session/steamos-manager-xdg.conf" "$xdg" || manager_changed=true
    install -Dm644 "$prefix/session/steamos-manager-xdg.conf" "$xdg"
    install -Dm644 -t "$HOME/.local/share/applications" "$prefix/share/frame-shell.desktop"
    install -Dm644 "$prefix/share/frame-shell.png" "$HOME/.local/share/icons/hicolor/256x256/apps/frame-shell.png"
    mkdir -p "$HOME/.local/bin"
    ln -sfn "$prefix/bin/wayvrctl" "$HOME/.local/bin/wayvrctl"
    systemctl --user daemon-reload
    systemctl --user enable --quiet frame-shell.service

    # The session entry SDDM and steamos-manager start Monado from; SteamOS updates keep only the listed /etc paths
    if ! cmp -s "$prefix/session/monado.desktop" /etc/frame-monado/wayland-sessions/monado.desktop ||
        ! cmp -s "$prefix/session/zz-frame-monado-sddm.conf" /etc/sddm.conf.d/zz-frame-monado.conf ||
        ! cmp -s "$prefix/session/frame-monado-keep.conf" /etc/atomic-update.conf.d/frame-monado.conf; then
        echo "Installing the Monado session entry in /etc (sudo):"
        sudo install -Dm644 "$prefix/session/monado.desktop" /etc/frame-monado/wayland-sessions/monado.desktop
        sudo install -Dm644 "$prefix/session/zz-frame-monado-sddm.conf" /etc/sddm.conf.d/zz-frame-monado.conf
        sudo install -Dm644 "$prefix/session/frame-monado-keep.conf" /etc/atomic-update.conf.d/frame-monado.conf
    fi
    # Gamepad mode's virtual DualSense needs /dev/uhid
    if ! cmp -s "$prefix/share/70-frame-shell-uhid.rules" /etc/udev/rules.d/70-frame-shell-uhid.rules ||
        ! cmp -s "$prefix/share/frame-shell-keep.conf" /etc/atomic-update.conf.d/frame-shell.conf; then
        echo "Installing the gamepad mode udev rule in /etc (sudo):"
        sudo install -Dm644 "$prefix/share/70-frame-shell-uhid.rules" /etc/udev/rules.d/70-frame-shell-uhid.rules
        sudo install -Dm644 "$prefix/share/frame-shell-keep.conf" /etc/atomic-update.conf.d/frame-shell.conf
        sudo udevadm control --reload-rules
        sudo udevadm trigger --action=change --subsystem-match=misc --sysname-match=uhid
    fi
    if "$manager_changed"; then systemctl --user restart steamos-manager.service; fi
    local session
    session=$(steamosctl get-default-desktop-session)
    if [ "$session" != monado.desktop ]; then
        echo "$session" >"$prefix/previous-desktop-session"
        steamosctl set-default-desktop-session monado.desktop
    fi
}

unintegrate() {
    systemctl --user disable --quiet frame-shell.service 2>/dev/null || true
    rm -f "${monado_units[@]/#/$units/}" "$units/frame-shell.service"
    rm -rf "$units/monado-session-fallback.service.d" "$units/steamos-manager.service.d/frame-monado.conf"
    rmdir "$units/steamos-manager.service.d" "$units" "${units%/user}" 2>/dev/null || true
    rm -f "$HOME/.local/share/applications/frame-shell.desktop" "$HOME/.local/share/icons/hicolor/256x256/apps/frame-shell.png"
    if [ "$(readlink "$HOME/.local/bin/wayvrctl")" = "$prefix/bin/wayvrctl" ]; then rm -f "$HOME/.local/bin/wayvrctl"; fi
    systemctl --user daemon-reload
    systemctl --user restart steamos-manager.service
    if [ "$(steamosctl get-default-desktop-session)" = monado.desktop ]; then
        steamosctl set-default-desktop-session "$(cat "$prefix/previous-desktop-session" 2>/dev/null || echo plasma.desktop)"
    fi
    if [ -e /etc/frame-monado ] || [ -e /etc/sddm.conf.d/zz-frame-monado.conf ] ||
        [ -e /etc/atomic-update.conf.d/frame-monado.conf ]; then
        echo "Removing the Monado session entry from /etc (sudo):"
        sudo rm -rf /etc/frame-monado /etc/sddm.conf.d/zz-frame-monado.conf /etc/atomic-update.conf.d/frame-monado.conf
    fi
    if [ -e /etc/udev/rules.d/70-frame-shell-uhid.rules ] || [ -e /etc/atomic-update.conf.d/frame-shell.conf ]; then
        echo "Removing the gamepad mode udev rule from /etc (sudo):"
        sudo rm -f /etc/udev/rules.d/70-frame-shell-uhid.rules /etc/atomic-update.conf.d/frame-shell.conf
        sudo udevadm control --reload-rules
    fi
}

install_bundle() {
    local bundle
    bundle=$(cd "$(dirname "$0")" && pwd)
    [ -x "$bundle/bin/frame-shell-client" ] && [ -x "$bundle/monado/bin/monado-service" ] || {
        echo "Run install.sh from an unpacked frame-shell bundle" >&2; exit 1;
    }
    [ -d /opt/steamvr/drivers/cv ] || { echo "SteamVR's Frame driver is missing: is this a Steam Frame?" >&2; exit 1; }
    [ "$bundle" != "$prefix" ] || { echo "Run the installer from a new bundle, not the installed copy" >&2; exit 1; }
    local steamos
    steamos=$(. /etc/os-release && echo "${VERSION_ID:-unknown}")
    if [ "$steamos" != "$steamos_target" ]; then
        echo "Warning: this release targets SteamOS $steamos_target (beta), this headset runs $steamos." >&2
    fi
    # Libraries this SteamOS lacks (the bundle uses SteamOS's own where it can)
    local binary report
    for binary in "$bundle/bin/frame-shell-client" "$bundle/bin/wayvr" "$bundle/lib/libwayvr.so" "$bundle/monado/bin/monado-service"; do
        report=$(LD_LIBRARY_PATH="$bundle/lib:$bundle/monado/lib" ldd "$binary" 2>&1) || true
        if [[ "$report" == *"not found"* ]]; then
            echo "This SteamOS lacks libraries for ${binary#"$bundle/"}:" >&2
            grep "not found" <<<"$report" >&2
            exit 1
        fi
    done

    # Monado restarts only when it changed; the shell always does
    local restart_monado=false monado_changed=true
    if diff -rq "$bundle/monado" "$prefix/monado" >/dev/null 2>&1 && diff -rq "$bundle/session" "$prefix/session" >/dev/null 2>&1; then
        monado_changed=false
    fi
    if in_monado_session && systemctl --user is-active --quiet monado.service; then restart_monado=true; fi
    if "$monado_changed"; then stop_monado; else systemctl --user stop frame-shell.service 2>/dev/null || true; fi
    trap 'rm -f "$marker"' EXIT

    mkdir -p "$prefix"
    local part
    for part in bin lib monado session share LICENSES; do
        rm -rf "${prefix:?}/$part"
        cp -a "$bundle/$part" "$prefix/"
    done
    cp -a "$bundle/install.sh" "$bundle/VERSION" "$bundle"/*.pte "$prefix/"
    integrate

    if in_monado_session; then
        if "$restart_monado" || ! systemctl --user is-active --quiet monado.service; then
            systemctl --user start monado.service
        else
            systemctl --user start frame-shell.service
        fi
    fi
    echo "Installed frame-shell $(cat "$prefix/VERSION")."
    in_monado_session || echo 'Start it with "Frame Shell" in SteamVR'"'"'s Launch Program menu (or steamosctl switch-to-desktop-mode).'
}

install_monado() {
    local dir=$1
    [ -x "$dir/monado/bin/monado-service" ] && [ -d "$dir/session" ] || { echo "$dir has no monado/ and session/" >&2; exit 1; }
    local restart=false
    if in_monado_session && systemctl --user is-active --quiet monado.service; then restart=true; fi
    stop_monado
    trap 'rm -f "$marker"' EXIT
    rm -rf "${prefix:?}/monado" "$prefix/session"
    cp -a "$dir/monado" "$dir/session" "$prefix/"
    integrate
    if "$restart"; then systemctl --user start monado.service; fi
}

uninstall() {
    local purge=false
    [ "${1:-}" = --purge ] && purge=true
    if in_monado_session; then
        echo "Switching back to SteamVR first"
        steamosctl switch-to-game-mode
        for _ in $(seq 60); do in_monado_session || break; sleep 0.5; done
    fi
    stop_monado
    trap 'rm -f "$marker"' EXIT
    unintegrate
    rm -rf "${prefix:?}" "$HOME/.local/state/frame-shell/bundle" "$HOME/.local/state/frame-monado/deploy"
    if "$purge"; then rm -rf "${user_data[@]}"; fi
    echo "Uninstalled frame-shell$("$purge" && echo ", with its settings" || true)."
}

case "${1:-install}" in
    install) install_bundle ;;
    uninstall) uninstall "${2:-}" ;;
    monado) install_monado "${2:?install.sh monado DIR}" ;;
    *) sed -n '2,6p' "$0" >&2; exit 1 ;;
esac
