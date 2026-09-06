desktop_bin := "/usr/local/bin/focaldesk-desktop"
focaldm_greeter_bin := "/usr/libexec/focaldm-greeter"

build:
    cargo build

nested-smoke:
    bash scripts/nested-smoke.sh

nested-smoke-no-build:
    bash scripts/nested-smoke.sh --no-build

# Install Fedora packages needed to compile the patched capture stack.
install-wide-gamut-capture-build-deps:
    sudo dnf install -y cmake git meson ninja-build
    sudo dnf builddep -y obs-studio xdg-desktop-portal-wlr

# Fetch, patch, and compile the OBS 32.1.1 wide-gamut SDR capture stack.
build-wide-gamut-capture obs_source="target/wide-gamut-capture/obs-studio-32.1.1" xdpw_source="target/wide-gamut-capture/xdg-desktop-portal-wlr-0.8.1":
    #!/usr/bin/env bash
    set -euo pipefail
    obs_source="$(realpath -m "{{ obs_source }}")"
    xdpw_source="$(realpath -m "{{ xdpw_source }}")"
    if [[ ! -d "$obs_source/.git" ]]; then
        mkdir -p "$(dirname "$obs_source")"
        git clone --branch 32.1.1 --depth 1 --recurse-submodules --shallow-submodules \
            https://github.com/obsproject/obs-studio.git "$obs_source"
    fi
    if [[ ! -d "$xdpw_source/.git" ]]; then
        mkdir -p "$(dirname "$xdpw_source")"
        git clone --branch v0.8.1 --depth 1 \
            https://github.com/emersion/xdg-desktop-portal-wlr.git "$xdpw_source"
    fi
    bash scripts/apply-wide-gamut-capture-patches.sh "$obs_source" "$xdpw_source"
    cmake -S "$obs_source" -B "$obs_source/build-focaldesk" -G Ninja \
        -DCMAKE_BUILD_TYPE=RelWithDebInfo \
        -DENABLE_AJA=OFF \
        -DENABLE_BROWSER=OFF \
        -DENABLE_WEBRTC=OFF
    cmake --build "$obs_source/build-focaldesk"
    if [[ -f "$xdpw_source/build-focaldesk/build.ninja" ]]; then
        meson setup --reconfigure --buildtype=release \
            "$xdpw_source/build-focaldesk" "$xdpw_source"
    else
        meson setup --buildtype=release \
            "$xdpw_source/build-focaldesk" "$xdpw_source"
    fi
    meson compile -C "$xdpw_source/build-focaldesk"
    echo "Built patched OBS and xdg-desktop-portal-wlr in their build-focaldesk directories."
    echo "This recipe does not install them or replace the system packages."

release-desktop:
    cargo build --release -p focaldesk-desktop

release-server:
    cargo build --release -p focaldesk-server

release-powerd:
    cargo build --release -p focaldesk-powerd

release-notificationsd:
    cargo build --release -p focaldesk-notificationsd

release-updatesd:
    cargo build --release -p focaldesk-updatesd

release-dialogd:
    cargo build --release -p focaldesk-dialogd

release-controlsd:
    cargo build --release -p focaldesk-controlsd

release-polkitd:
    cargo build --release -p focaldesk-polkitd

release-launchd:
    cargo build --release -p focal-launchd

release-portal:
    cargo build --release -p focaldesk-portal

release-automation:
    cargo build --release -p focaldesk-automation

release-mcp:
    cargo build --release -p focaldesk-mcp

release-focaldm-greeter:
    cargo build --release -p focaldm-greeter

release-focaldmd:
    cargo build --release -p focaldmd

release-focald-secrets:
    cargo build --release -p focald-secrets -p pam-focald-secrets

release-shell:
    cargo build --release -p focaldesk-system-rail -p focaldesk-task-shelf

install-server-service:
    cargo build --release -p focaldesk-server
    install -Dm755 target/release/focaldesk-server "$HOME/.local/bin/focaldesk-server"
    install -Dm644 packaging/systemd/user/focaldesk-server.service "$HOME/.config/systemd/user/focaldesk-server.service"
    # One-time migration: focaldesk-server.service moved from default.target to graphical-session.target.
    rm -f "$HOME/.config/systemd/user/default.target.wants/focaldesk-server.service"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-server.service || echo "Skipping systemd user enable: no user bus available"

install-session-target:
    install -Dm644 packaging/systemd/user/focaldesk-session.target "$HOME/.config/systemd/user/focaldesk-session.target"
    # Migrate services formerly enabled directly under the shared desktop target.
    rm -f "$HOME/.config/systemd/user/graphical-session.target.wants/"focaldesk-*.service
    rm -f "$HOME/.config/systemd/user/graphical-session.target.wants/focal-launchd.service"
    rm -f "$HOME/.config/systemd/user/focaldesk-session.target.wants/focaldesk-polkitd.service"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"

install-shell-services: install-session-target
    cargo build --release -p focaldesk-system-rail -p focaldesk-task-shelf
    install -Dm755 target/release/focaldesk-system-rail "$HOME/.local/bin/focaldesk-system-rail"
    install -Dm755 target/release/focaldesk-task-shelf "$HOME/.local/bin/focaldesk-task-shelf"
    install -Dm644 packaging/systemd/user/focaldesk-system-rail.service "$HOME/.config/systemd/user/focaldesk-system-rail.service"
    install -Dm644 packaging/systemd/user/focaldesk-task-shelf.service "$HOME/.config/systemd/user/focaldesk-task-shelf.service"
    systemctl --user disable --now focaldesk-panel.service focaldesk-dock.service || true
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-system-rail.service focaldesk-task-shelf.service || echo "Skipping GTK shell enable: no user bus available"

install-services: install-session-target install-shell-services install-server-service install-power-service install-notifications-service install-updates-service install-dialog-service install-control-service install-launch-service install-settings-service install-polkit-service install-portal install-focald-voice install-focald-speech install-focald-mic

install-secrets-service:
    cargo build --release -p focald-secrets
    install -Dm755 target/release/focald-secrets "$HOME/.local/bin/focald-secrets"
    install -Dm755 target/release/focald-secrets-keytool "$HOME/.local/bin/focald-secrets-keytool"
    install -Dm755 target/release/focald-secrets-import-gnome-keyring "$HOME/.local/bin/focald-secrets-import-gnome-keyring"
    install -Dm644 packaging/systemd/user/focald-secrets.service "$HOME/.config/systemd/user/focald-secrets.service"
    install -Dm644 packaging/systemd/user/focald-secrets.socket "$HOME/.config/systemd/user/focald-secrets.socket"
    install -Dm644 packaging/systemd/user/focald-secrets-import.service "$HOME/.config/systemd/user/focald-secrets-import.service"
    test -e "$HOME/.config/focaldesk/secrets-acl.toml" || install -Dm600 packaging/focaldesk/secrets-acl.toml "$HOME/.config/focaldesk/secrets-acl.toml"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    echo "Development unit installed; set FOCALD_SECRETS_KEYFILE explicitly before manual unlock/start"

install-power-service:
    cargo build --release -p focaldesk-powerd
    install -Dm755 target/release/focaldesk-powerd "$HOME/.local/bin/focaldesk-powerd"
    install -Dm644 packaging/systemd/user/focaldesk-powerd.service "$HOME/.config/systemd/user/focaldesk-powerd.service"
    # One-time migration: focaldesk-powerd.service moved from default.target to graphical-session.target.
    rm -f "$HOME/.config/systemd/user/default.target.wants/focaldesk-powerd.service"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-powerd.service || echo "Skipping systemd user enable: no user bus available"

install-notifications-service:
    cargo build --release -p focaldesk-notificationsd
    install -Dm755 target/release/focaldesk-notificationsd "$HOME/.local/bin/focaldesk-notificationsd"
    install -Dm644 packaging/systemd/user/focaldesk-notificationsd.service "$HOME/.config/systemd/user/focaldesk-notificationsd.service"
    # One-time migration: focaldesk-notificationsd.service moved from default.target to graphical-session.target.
    rm -f "$HOME/.config/systemd/user/default.target.wants/focaldesk-notificationsd.service"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-notificationsd.service || echo "Skipping systemd user enable: no user bus available"

install-updates-service:
    cargo build --release -p focaldesk-updatesd
    install -Dm755 target/release/focaldesk-updatesd "$HOME/.local/bin/focaldesk-updatesd"
    install -Dm644 packaging/systemd/user/focaldesk-updatesd.service "$HOME/.config/systemd/user/focaldesk-updatesd.service"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-updatesd.service || echo "Skipping systemd user enable: no user bus available"

install-dialog-service:
    cargo build --release -p focaldesk-dialogd
    install -Dm755 target/release/focaldesk-dialogd "$HOME/.local/bin/focaldesk-dialogd"
    install -Dm644 packaging/systemd/user/focaldesk-dialogd.service "$HOME/.config/systemd/user/focaldesk-dialogd.service"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-dialogd.service || echo "Skipping systemd user enable: no user bus available"

install-control-service:
    cargo build --release -p focaldesk-controlsd
    install -Dm755 target/release/focaldesk-controlsd "$HOME/.local/bin/focaldesk-controlsd"
    install -Dm644 packaging/systemd/user/focaldesk-controlsd.service "$HOME/.config/systemd/user/focaldesk-controlsd.service"
    # One-time migration: focaldesk-controlsd.service moved from default.target to graphical-session.target.
    rm -f "$HOME/.config/systemd/user/default.target.wants/focaldesk-controlsd.service"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-controlsd.service || echo "Skipping systemd user enable: no user bus available"

install-launch-service:
    cargo build --release -p focal-launchd
    install -Dm755 target/release/focal-launchd "$HOME/.local/bin/focal-launchd"
    install -Dm644 packaging/systemd/user/focal-launchd.service "$HOME/.config/systemd/user/focal-launchd.service"
    # One-time migration: focal-launchd.service moved from default.target to graphical-session.target.
    rm -f "$HOME/.config/systemd/user/default.target.wants/focal-launchd.service"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focal-launchd.service || echo "Skipping systemd user enable: no user bus available"

install-settings-service:
    cargo build --release -p focaldesk-ipc
    install -Dm755 target/release/focaldesk-settingsd "$HOME/.local/bin/focaldesk-settingsd"
    install -Dm644 packaging/systemd/user/focaldesk-settingsd.service "$HOME/.config/systemd/user/focaldesk-settingsd.service"
    # One-time migration: focaldesk-settingsd.service moved from default.target to graphical-session.target.
    rm -f "$HOME/.config/systemd/user/default.target.wants/focaldesk-settingsd.service"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-settingsd.service || echo "Skipping systemd user enable: no user bus available"

install-polkit-service:
    cargo build --release -p focaldesk-polkitd
    install -Dm755 target/release/focaldesk-polkitd "$HOME/.local/bin/focaldesk-polkitd"
    systemctl --user disable --now focaldesk-polkitd.service || echo "Skipping stale polkit service cleanup: no user bus available"

install-portal:
    cargo build --release -p focaldesk-portal
    install -Dm755 target/release/focaldesk-portal "$HOME/.local/bin/focaldesk-portal"
    install -Dm644 packaging/systemd/user/focaldesk-portald.service "$HOME/.config/systemd/user/focaldesk-portald.service"
    install -Dm644 packaging/xdg-desktop-portal/focaldesk.portal "$HOME/.local/share/xdg-desktop-portal/portals/focaldesk.portal"
    install -Dm644 packaging/xdg-desktop-portal/focaldesk-portals.conf "$HOME/.local/share/xdg-desktop-portal/focaldesk-portals.conf"
    mkdir -p "$HOME/.config/xdg-desktop-portal-wlr"
    target/release/focaldesk-portal --print-xdpw-config > "$HOME/.config/xdg-desktop-portal-wlr/config"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user restart focaldesk-portald.service || echo "Restart the FocalDesk portal backend after logging into FocalDesk"
    systemctl --user restart xdg-desktop-portal xdg-desktop-portal-wlr || echo "Restart portal services manually after logging into FocalDesk"

install-automation-service:
    cargo build --release -p focaldesk-automation
    install -Dm755 target/release/focaldesk-automation "$HOME/.local/bin/focaldesk-automation"
    install -Dm644 packaging/systemd/user/focaldesk-automation.service "$HOME/.config/systemd/user/focaldesk-automation.service"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-automation.service || echo "Skipping systemd user enable: no user bus available"

install-updatesd-service:
    cargo build --release -p focaldesk-updatesd
    install -Dm755 target/release/focaldesk-updatesd "$HOME/.local/bin/focaldesk-updatesd"
    install -Dm644 packaging/systemd/user/focaldesk-updatesd.service "$HOME/.config/systemd/user/focaldesk-updatesd.service"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-updatesd.service || echo "Skipping systemd user enable: no user bus available"

# MCP clients launch this stdio server on demand; it is not a systemd service.
install-mcp:
    cargo build --release -p focaldesk-mcp
    install -Dm755 target/release/focaldesk-mcp "$HOME/.local/bin/focaldesk-mcp"

install-files:
    cargo build --release -p focaldesk-files
    sudo install -Dm755 target/release/focaldesk-files /usr/local/bin/focaldesk-files

install-settings:
    cargo build --release -p focaldesk-settings
    sudo install -Dm755 target/release/focaldesk-settings /usr/local/bin/focaldesk-settings

install-ai-console:
    cargo build --release -p focaldesk-ai-console
    sudo install -Dm755 target/release/focaldesk-ai-console /usr/local/bin/focaldesk-ai-console

# Install the AI IPC backend used at session boot and the console launched from
# the desktop. The console is an application, so it is intentionally not a
# long-running systemd service of its own.
install-ai: install-dialog-service install-server-service install-ai-console

# Fedora system-installed variant: use the /usr/bin daemon and the Fedora
# user-unit path. It is still managed by systemctl --user because it belongs
# to the logged-in graphical desktop session.
install-ai-fedora: migrate-ai-user-units install-dialog-service-fedora install-server-service-fedora install-ai-console

# Remove the older per-user development units before installing Fedora's
# system-provided user units. A unit in ~/.config/systemd/user overrides the
# matching unit in /usr/lib/systemd/user, even when the latter is newer.
migrate-ai-user-units:
    systemctl --user disable --now focaldesk-server.service focaldesk-dialogd.service 2>/dev/null || true
    rm -f "$HOME/.config/systemd/user/focaldesk-server.service" "$HOME/.config/systemd/user/focaldesk-dialogd.service"
    systemctl --user daemon-reload || true

install-focald-voice:
    cargo build --release -p focald-voice
    install -Dm755 target/release/focald-voice "$HOME/.local/bin/focald-voice"
    install -Dm644 packaging/systemd/user/focald-voice.service "$HOME/.config/systemd/user/focald-voice.service"
    systemctl --user daemon-reload
    systemctl --user enable --now focald-voice.service
    systemctl --user restart focald-voice.service

install-focald-speech:
    cargo build --release -p focald-speech
    install -Dm755 target/release/focald-speech "$HOME/.local/bin/focald-speech"
    install -Dm644 packaging/systemd/user/focald-speech.service "$HOME/.config/systemd/user/focald-speech.service"
    systemctl --user daemon-reload
    systemctl --user enable --now focald-speech.service
    systemctl --user restart focald-speech.service

install-focald-mic:
    cargo build --release -p focald-mic
    install -Dm755 target/release/focald-mic "$HOME/.local/bin/focald-mic"
    install -Dm644 packaging/systemd/user/focald-mic.service "$HOME/.config/systemd/user/focald-mic.service"
    systemctl --user daemon-reload
    systemctl --user enable --now focald-mic.service
    systemctl --user restart focald-mic.service

install-polkit:
    cargo build --release -p focaldesk-polkitd
    sudo install -Dm755 target/release/focaldesk-polkitd /usr/libexec/focaldesk/focaldesk-polkitd

install-launcher:
    cargo build --release -p focaldesk-launcher
    sudo install -Dm755 target/release/focaldesk-launcher /usr/local/bin/focaldesk-launcher

install-focaldm-greeter: release-focaldm-greeter
    sudo install -Dm755 target/release/focaldm-greeter "{{focaldm_greeter_bin}}"

install-focaldmd-fedora: release-focaldmd release-focaldm-greeter install-focaldm-pam-fedora
    sudo install -Dm755 target/release/focaldmd /usr/sbin/focaldmd
    # pam_selinux opens the authenticated desktop in the user's SELinux
    # domain.  The daemon must run in xdm_t for that transition to be allowed;
    # a generic bin_t install leaves systemd running it as
    # unconfined_service_t and every successful login fails at execve(EACCES).
    # Fedora's SELinux policy maps /usr/sbin to /usr/bin, so semanage needs
    # the canonical /usr/bin path even though the executable lives in sbin.
    sudo semanage fcontext -a -t xdm_exec_t /usr/bin/focaldmd 2>/dev/null || sudo semanage fcontext -m -t xdm_exec_t /usr/bin/focaldmd
    sudo restorecon -v /usr/sbin/focaldmd
    sudo install -Dm755 target/release/focaldm-greeter "{{focaldm_greeter_bin}}"
    sudo install -Dm644 packaging/systemd/system/focaldmd.service /usr/lib/systemd/system/focaldmd.service
    sudo install -Dm644 packaging/sysusers.d/focaldesk.conf /usr/lib/sysusers.d/focaldesk.conf
    sudo install -Dm644 packaging/pam/focaldmd-greeter-fedora /etc/pam.d/focaldmd-greeter
    sudo install -Dm644 packaging/focaldesk/focaldmd.toml /etc/focaldmd.toml
    sudo systemd-sysusers /usr/lib/sysusers.d/focaldesk.conf
    sudo systemctl daemon-reload
    @echo "focaldmd is installed but not enabled. Keep another display manager available until login testing succeeds."

install-desktop: install-polkit
    cargo build --release -p focaldesk-desktop
    sudo install -Dm644 assets/themes/default.toml /usr/share/focaldesk/default.toml
    sudo install -Dm644 assets/wallpaper/focaldesk_wallpaper.png /usr/share/focaldesk/wallpaper/focaldesk_wallpaper.png
    @echo "Build artifact:"
    @md5sum target/release/focaldesk-desktop
    # mv over the running binary: direct install/cp to the live path gets "Text file busy".
    sudo install -Dm755 target/release/focaldesk-desktop "{{desktop_bin}}.new"
    sudo mv -f "{{desktop_bin}}.new" "{{desktop_bin}}"
    @echo "Installed to {{desktop_bin}}:"
    @md5sum "{{desktop_bin}}"
    @bash -c 'b=$(md5sum target/release/focaldesk-desktop | cut -d" " -f1); i=$(md5sum "{{desktop_bin}}" | cut -d" " -f1); test "$b" = "$i" && echo "md5 OK: $i" || { echo "md5 MISMATCH: build=$b installed=$i" >&2; exit 1; }'
    @if test -e /proc/driver/nvidia/version && rg -q 's2idle' /sys/power/mem_sleep; then sudo install -Dm644 packaging/systemd/sleep.conf.d/90-focaldesk-nvidia.conf /etc/systemd/sleep.conf.d/90-focaldesk-nvidia.conf && printf '%s\n' s2idle | sudo tee /sys/power/mem_sleep >/dev/null && systemd-analyze cat-config systemd/sleep.conf | rg 'MemorySleepMode=s2idle' && rg -q '\[s2idle\]' /sys/power/mem_sleep && echo "NVIDIA detected: installed and activated the s2idle suspend workaround."; fi

# Install or verify the workaround independently when troubleshooting NVIDIA
# resume. install-desktop applies the same guarded configuration automatically;
# other GPUs retain the platform's default sleep mode.
install-nvidia-suspend-workaround:
    test -e /proc/driver/nvidia/version
    rg -q 's2idle' /sys/power/mem_sleep
    sudo install -Dm644 packaging/systemd/sleep.conf.d/90-focaldesk-nvidia.conf /etc/systemd/sleep.conf.d/90-focaldesk-nvidia.conf
    printf '%s\n' s2idle | sudo tee /sys/power/mem_sleep >/dev/null
    systemd-analyze cat-config systemd/sleep.conf | rg 'MemorySleepMode=s2idle'
    rg -q '\[s2idle\]' /sys/power/mem_sleep
    @echo "NVIDIA suspend workaround installed and active; suspend will use s2idle."

install-desktop-session:
    sudo install -Dm644 packaging/wayland-sessions/focaldesk.desktop /usr/share/wayland-sessions/focaldesk.desktop
    sudo install -Dm644 packaging/systemd/user/focaldesk-session.target /usr/lib/systemd/user/focaldesk-session.target

install-server-service-fedora:
    cargo build --release -p focaldesk-server
    sudo install -Dm755 target/release/focaldesk-server /usr/bin/focaldesk-server
    sudo install -Dm644 packaging/systemd/user/focaldesk-server-fedora.service /usr/lib/systemd/user/focaldesk-server.service
    # One-time migration: focaldesk-server.service moved from default.target to graphical-session.target.
    rm -f "$HOME/.config/systemd/user/default.target.wants/focaldesk-server.service"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-server.service || echo "Skipping systemd user enable: no user bus available"

install-services-fedora: install-runtime-dir-fedora install-session-target-fedora install-shell-services-fedora install-server-service-fedora install-power-service-fedora install-notifications-service-fedora install-updates-service-fedora install-dialog-service-fedora install-control-service-fedora install-launch-service-fedora install-settings-service-fedora install-polkit-service-fedora install-portal-fedora install-voice-service-fedora install-speech-service-fedora install-mic-service-fedora

# Both the system credential socket and user-session IPC use this directory.
# Prepare it before starting user services so a directory created by PID 1
# cannot leave every desktop service with EACCES.
install-runtime-dir-fedora:
    sudo install -Dm644 packaging/systemd/system/focaldesk-runtime-dir@.service /usr/lib/systemd/system/focaldesk-runtime-dir@.service
    sudo systemctl daemon-reload
    if test ! -L "/run/user/$(id -u)/focaldesk" && test -d "/run/user/$(id -u)/focaldesk" && test "$(stat -c %u "/run/user/$(id -u)/focaldesk")" != "$(id -u)"; then sudo chown --no-dereference "$(id -u):$(id -g)" "/run/user/$(id -u)/focaldesk"; sudo chmod 0700 "/run/user/$(id -u)/focaldesk"; fi
    sudo systemctl restart "focaldesk-runtime-dir@$(id -u).service"

install-secrets-selinux-fedora:
    test -f /usr/share/selinux/devel/Makefile || { echo "Install selinux-policy-devel first" >&2; exit 1; }
    make -C packaging/selinux focaldesk_secrets.pp
    sudo semodule -i packaging/selinux/focaldesk_secrets.pp
    sudo restorecon -RFv "$HOME/.local/share/focaldesk"

install-secrets-service-fedora: install-secrets-selinux-fedora install-runtime-dir-fedora
    cargo build --release -p focald-secrets
    # Upgrade cleanup: a public socket must not start the broker before PAM has
    # staged its key. The root-only credential path now triggers startup.
    systemctl --user disable --now focald-secrets.service focald-secrets.socket || true
    rm -f "$HOME/.config/systemd/user/focaldesk-session.target.wants/focald-secrets.service"
    rm -f "$HOME/.config/systemd/user/focaldesk-session.target.wants/focald-secrets.socket"
    sudo rm -f /usr/lib/systemd/user/focald-secrets.service
    sudo rm -f /usr/lib/systemd/user/focald-secrets.socket
    sudo rm -f /usr/lib/systemd/system/user@.service.d/90-focaldesk-memlock.conf
    sudo systemctl stop "focald-secrets@$(id -u).socket" || true
    sudo install -Dm755 target/release/focald-secrets /usr/bin/focald-secrets
    sudo install -Dm755 target/release/focald-secrets-keytool /usr/bin/focald-secrets-keytool
    sudo install -Dm755 target/release/focald-secrets-import-gnome-keyring /usr/bin/focald-secrets-import-gnome-keyring
    sudo install -Dm644 packaging/systemd/user/focald-secrets-import-fedora.service /usr/lib/systemd/user/focald-secrets-import.service
    sudo install -Dm644 packaging/systemd/system/focald-secrets@.service /usr/lib/systemd/system/focald-secrets@.service
    sudo install -Dm644 packaging/systemd/system/focald-secrets@.path /usr/lib/systemd/system/focald-secrets@.path
    sudo rm -f /usr/lib/systemd/system/focald-secrets@.socket
    sudo install -Dm644 packaging/systemd/system/user@.service.d/90-focald-secrets.conf /usr/lib/systemd/system/user@.service.d/90-focald-secrets.conf
    # The credential-fed broker is started by its system path unit at login. A
    # session-bus activation entry cannot address a per-UID system unit and
    # would launch the daemon without its credential, so remove stale installs.
    sudo rm -f /usr/share/dbus-1/services/org.freedesktop.secrets.service
    rm -f "$HOME/.local/share/dbus-1/services/org.freedesktop.secrets.service"
    test -e /etc/focaldesk/secrets-acl.toml || sudo install -Dm644 packaging/focaldesk/secrets-acl.toml /etc/focaldesk/secrets-acl.toml
    sudo systemctl daemon-reload
    sudo systemctl reset-failed "focald-secrets@$(id -u).service" "focald-secrets@$(id -u).socket" || true
    sudo systemctl start "focald-secrets@$(id -u).path"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    echo "The PAM session hook unlocks focald-secrets@UID.service through its credential path"

# Install FocalDesk's PAM module without changing the active login policy.
install-secrets-pam-fedora:
    cargo build --release -p pam-focald-secrets
    sudo install -Dm755 target/release/libpam_focald_secrets.so /usr/lib64/security/pam_focald_secrets.so

# Install the complete focaldmd login policy. The native Focaldesk vault also
# provides the standard Secret Service API used by Chrome and other clients.
install-focaldm-pam-fedora: install-secrets-pam-fedora
    test -e /usr/lib64/security/pam_focald_secrets.so
    ! rg -q 'pam_gnome_keyring\\.so' packaging/pam/focaldmd-fedora
    rg -q '^auth[[:space:]]+optional[[:space:]]+pam_focald_secrets\\.so$' packaging/pam/focaldmd-fedora
    rg -q '^password[[:space:]]+optional[[:space:]]+pam_focald_secrets\\.so$' packaging/pam/focaldmd-fedora
    rg -q '^session[[:space:]]+optional[[:space:]]+pam_focald_secrets\\.so$' packaging/pam/focaldmd-fedora
    sudo install -Dm644 packaging/pam/focaldmd-fedora /etc/pam.d/focaldmd

install-session-target-fedora:
    sudo install -Dm644 packaging/systemd/user/focaldesk-session.target /usr/lib/systemd/user/focaldesk-session.target
    # Migrate services formerly enabled directly under the shared desktop target.
    rm -f "$HOME/.config/systemd/user/graphical-session.target.wants/"focaldesk-*.service
    rm -f "$HOME/.config/systemd/user/graphical-session.target.wants/focal-launchd.service"
    rm -f "$HOME/.config/systemd/user/focaldesk-session.target.wants/focaldesk-polkitd.service"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"

install-shell-services-fedora: install-session-target-fedora
    cargo build --release -p focaldesk-system-rail -p focaldesk-task-shelf
    sudo install -Dm755 target/release/focaldesk-system-rail /usr/bin/focaldesk-system-rail
    sudo install -Dm755 target/release/focaldesk-task-shelf /usr/bin/focaldesk-task-shelf
    sudo install -Dm644 packaging/systemd/user/focaldesk-system-rail-fedora.service /usr/lib/systemd/user/focaldesk-system-rail.service
    sudo install -Dm644 packaging/systemd/user/focaldesk-task-shelf-fedora.service /usr/lib/systemd/user/focaldesk-task-shelf.service
    systemctl --user disable --now focaldesk-panel.service focaldesk-dock.service || true
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-system-rail.service focaldesk-task-shelf.service || echo "Skipping GTK shell enable: no user bus available"

install-power-service-fedora:
    cargo build --release -p focaldesk-powerd
    sudo install -Dm755 target/release/focaldesk-powerd /usr/bin/focaldesk-powerd
    sudo install -Dm644 packaging/systemd/user/focaldesk-powerd-fedora.service /usr/lib/systemd/user/focaldesk-powerd.service
    # One-time migration: focaldesk-powerd.service moved from default.target to graphical-session.target.
    rm -f "$HOME/.config/systemd/user/default.target.wants/focaldesk-powerd.service"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-powerd.service || echo "Skipping systemd user enable: no user bus available"

install-updatesd-service-fedora:
    cargo build --release -p focaldesk-updatesd
    sudo install -Dm755 target/release/focaldesk-updatesd /usr/bin/focaldesk-updatesd
    install -Dm644 packaging/systemd/user/focaldesk-updatesd.service /usr/lib/systemd/user/focaldesk-updatesd.service
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-updatesd.service || echo "Skipping systemd user enable: no user bus available"

install-notifications-service-fedora:
    cargo build --release -p focaldesk-notificationsd
    sudo install -Dm755 target/release/focaldesk-notificationsd /usr/bin/focaldesk-notificationsd
    sudo install -Dm644 packaging/systemd/user/focaldesk-notificationsd-fedora.service /usr/lib/systemd/user/focaldesk-notificationsd.service
    # One-time migration: focaldesk-notificationsd.service moved from default.target to graphical-session.target.
    rm -f "$HOME/.config/systemd/user/default.target.wants/focaldesk-notificationsd.service"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-notificationsd.service || echo "Skipping systemd user enable: no user bus available"

install-updates-service-fedora:
    cargo build --release -p focaldesk-updatesd
    sudo install -Dm755 target/release/focaldesk-updatesd /usr/bin/focaldesk-updatesd
    sudo install -Dm644 packaging/systemd/user/focaldesk-updatesd-fedora.service /usr/lib/systemd/user/focaldesk-updatesd.service
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-updatesd.service || echo "Skipping systemd user enable: no user bus available"

install-dialog-service-fedora:
    cargo build --release -p focaldesk-dialogd
    sudo install -Dm755 target/release/focaldesk-dialogd /usr/bin/focaldesk-dialogd
    sudo install -Dm644 packaging/systemd/user/focaldesk-dialogd-fedora.service /usr/lib/systemd/user/focaldesk-dialogd.service
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-dialogd.service || echo "Skipping systemd user enable: no user bus available"

install-control-service-fedora:
    cargo build --release -p focaldesk-controlsd
    sudo install -Dm755 target/release/focaldesk-controlsd /usr/bin/focaldesk-controlsd
    sudo install -Dm644 packaging/systemd/user/focaldesk-controlsd-fedora.service /usr/lib/systemd/user/focaldesk-controlsd.service
    # One-time migration: focaldesk-controlsd.service moved from default.target to graphical-session.target.
    rm -f "$HOME/.config/systemd/user/default.target.wants/focaldesk-controlsd.service"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-controlsd.service || echo "Skipping systemd user enable: no user bus available"

install-automation-service-fedora:
    cargo build --release -p focaldesk-automation
    sudo install -Dm755 target/release/focaldesk-automation /usr/bin/focaldesk-automation
    sudo install -Dm644 packaging/systemd/user/focaldesk-automation-fedora.service /usr/lib/systemd/user/focaldesk-automation.service
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-automation.service || echo "Skipping systemd user enable: no user bus available"

install-launch-service-fedora:
    cargo build --release -p focal-launchd
    sudo install -Dm755 target/release/focal-launchd /usr/bin/focal-launchd
    sudo install -Dm644 packaging/systemd/user/focal-launchd-fedora.service /usr/lib/systemd/user/focal-launchd.service
    # One-time migration: focal-launchd.service moved from default.target to graphical-session.target.
    rm -f "$HOME/.config/systemd/user/default.target.wants/focal-launchd.service"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focal-launchd.service || echo "Skipping systemd user enable: no user bus available"

install-settings-service-fedora:
    cargo build --release -p focaldesk-ipc
    sudo install -Dm755 target/release/focaldesk-settingsd /usr/bin/focaldesk-settingsd
    sudo install -Dm644 packaging/systemd/user/focaldesk-settingsd-fedora.service /usr/lib/systemd/user/focaldesk-settingsd.service
    # One-time migration: focaldesk-settingsd.service moved from default.target to graphical-session.target.
    rm -f "$HOME/.config/systemd/user/default.target.wants/focaldesk-settingsd.service"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-settingsd.service || echo "Skipping systemd user enable: no user bus available"

install-polkit-service-fedora:
    cargo build --release -p focaldesk-polkitd
    sudo install -Dm755 target/release/focaldesk-polkitd /usr/bin/focaldesk-polkitd
    systemctl --user disable --now focaldesk-polkitd.service || echo "Skipping stale polkit service cleanup: no user bus available"

install-portal-fedora:
    cargo build --release -p focaldesk-portal
    sudo install -Dm755 target/release/focaldesk-portal /usr/bin/focaldesk-portal
    sudo install -Dm644 packaging/systemd/user/focaldesk-portald-fedora.service /usr/lib/systemd/user/focaldesk-portald.service
    sudo install -Dm644 packaging/dbus/org.freedesktop.impl.portal.desktop.focaldesk.service /usr/share/dbus-1/services/org.freedesktop.impl.portal.desktop.focaldesk.service
    sudo install -Dm644 packaging/xdg-desktop-portal/focaldesk.portal /usr/share/xdg-desktop-portal/portals/focaldesk.portal
    sudo install -Dm644 packaging/xdg-desktop-portal/focaldesk-portals.conf /usr/share/xdg-desktop-portal/focaldesk-portals.conf
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user restart focaldesk-portald.service || echo "Restart the FocalDesk portal backend after logging into FocalDesk"
    systemctl --user restart xdg-desktop-portal xdg-desktop-portal-wlr || echo "Restart portal services manually after logging into FocalDesk"

install-voice-service-fedora:
    cargo build --release -p focald-voice
    sudo install -Dm755 target/release/focald-voice /usr/bin/focald-voice
    sudo install -Dm644 packaging/systemd/user/focald-voice-fedora.service /usr/lib/systemd/user/focald-voice.service
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focald-voice.service || echo "Skipping systemd user enable: no user bus available"

install-speech-service-fedora:
    cargo build --release -p focald-speech
    sudo install -Dm755 target/release/focald-speech /usr/bin/focald-speech
    sudo install -Dm644 packaging/systemd/user/focald-speech-fedora.service /usr/lib/systemd/user/focald-speech.service
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focald-speech.service || echo "Skipping systemd user enable: no user bus available"

install-mic-service-fedora:
    cargo build --release -p focald-mic
    sudo install -Dm755 target/release/focald-mic /usr/bin/focald-mic
    sudo install -Dm644 packaging/systemd/user/focald-mic-fedora.service /usr/lib/systemd/user/focald-mic.service
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focald-mic.service || echo "Skipping systemd user enable: no user bus available"

run:
    cargo run

fmt:
    cargo fmt

lint:
    cargo clippy -- -D warnings

test:
    cargo test
