desktop_bin := "/usr/local/bin/focaldesk-desktop"
focaldm_greeter_bin := "/usr/libexec/focaldm-greeter"

build:
    cargo build

release-desktop:
    cargo build --release -p focaldesk-desktop

release-server:
    cargo build --release -p focaldesk-server

release-powerd:
    cargo build --release -p focaldesk-powerd

release-notificationsd:
    cargo build --release -p focaldesk-notificationsd

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

release-focaldm-greeter:
    cargo build --release -p focaldm-greeter

release-focald-secrets:
    cargo build --release -p focald-secrets -p pam-focald-secrets

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

install-services: install-session-target install-server-service install-power-service install-notifications-service install-dialog-service install-control-service install-automation-service install-launch-service install-settings-service install-polkit-service install-focald-voice install-focald-speech install-focald-mic

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
    systemctl --user enable --now focald-secrets.socket || echo "Unlock the keyring, then start focald-secrets.socket"

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
    mkdir -p "$HOME/.config/xdg-desktop-portal-wlr"
    target/release/focaldesk-portal --print-xdpw-config > "$HOME/.config/xdg-desktop-portal-wlr/config"
    systemctl --user restart xdg-desktop-portal xdg-desktop-portal-wlr || echo "Restart portal services manually after logging into FocalDesk"

install-automation-service:
    cargo build --release -p focaldesk-automation
    install -Dm755 target/release/focaldesk-automation "$HOME/.local/bin/focaldesk-automation"
    install -Dm644 packaging/systemd/user/focaldesk-automation.service "$HOME/.config/systemd/user/focaldesk-automation.service"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-automation.service || echo "Skipping systemd user enable: no user bus available"

install-files:
    cargo build --release -p focaldesk-files
    sudo install -Dm755 target/release/focaldesk-files /usr/local/bin/focaldesk-files

install-settings:
    cargo build --release -p focaldesk-settings
    sudo install -Dm755 target/release/focaldesk-settings /usr/local/bin/focaldesk-settings

install-ai-console:
    cargo build --release -p focaldesk-ai-console
    sudo install -Dm755 target/release/focaldesk-ai-console /usr/local/bin/focaldesk-ai-console

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

install-desktop: install-polkit
    cargo build --release -p focaldesk-desktop
    @echo "Build artifact:"
    @md5sum target/release/focaldesk-desktop
    # mv over the running binary: direct install/cp to the live path gets "Text file busy".
    sudo install -Dm755 target/release/focaldesk-desktop "{{desktop_bin}}.new"
    sudo mv -f "{{desktop_bin}}.new" "{{desktop_bin}}"
    @echo "Installed to {{desktop_bin}}:"
    @md5sum "{{desktop_bin}}"
    @bash -c 'b=$(md5sum target/release/focaldesk-desktop | cut -d" " -f1); i=$(md5sum "{{desktop_bin}}" | cut -d" " -f1); test "$b" = "$i" && echo "md5 OK: $i" || { echo "md5 MISMATCH: build=$b installed=$i" >&2; exit 1; }'

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

install-services-fedora: install-session-target-fedora install-server-service-fedora install-power-service-fedora install-notifications-service-fedora install-dialog-service-fedora install-control-service-fedora install-automation-service-fedora install-launch-service-fedora install-settings-service-fedora install-polkit-service-fedora install-speech-service-fedora install-mic-service-fedora

install-secrets-service-fedora:
    cargo build --release -p focald-secrets
    sudo install -Dm755 target/release/focald-secrets /usr/bin/focald-secrets
    sudo install -Dm755 target/release/focald-secrets-keytool /usr/bin/focald-secrets-keytool
    sudo install -Dm755 target/release/focald-secrets-import-gnome-keyring /usr/bin/focald-secrets-import-gnome-keyring
    sudo install -Dm644 packaging/systemd/user/focald-secrets-fedora.service /usr/lib/systemd/user/focald-secrets.service
    sudo install -Dm644 packaging/systemd/user/focald-secrets.socket /usr/lib/systemd/user/focald-secrets.socket
    sudo install -Dm644 packaging/systemd/user/focald-secrets-import-fedora.service /usr/lib/systemd/user/focald-secrets-import.service
    test -e /etc/focaldesk/secrets-acl.toml || sudo install -Dm644 packaging/focaldesk/secrets-acl.toml /etc/focaldesk/secrets-acl.toml
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focald-secrets.socket || echo "Unlock the keyring, then start focald-secrets.socket"

# Install FocalDesk's PAM module without changing the active login policy.
install-secrets-pam-fedora:
    cargo build --release -p pam-focald-secrets
    sudo install -Dm755 target/release/libpam_focald_secrets.so /usr/lib64/security/pam_focald_secrets.so

# Install the complete focaldmd login policy. This includes both the native
# FocalDesk vault and GNOME Keyring auto-unlock used by Chrome and other
# Secret Service clients.
install-focaldm-pam-fedora: install-secrets-pam-fedora
    test -e /usr/lib64/security/pam_gnome_keyring.so
    rg -q '^auth[[:space:]]+optional[[:space:]]+pam_gnome_keyring\\.so$' packaging/pam/focaldmd-fedora
    rg -q '^session[[:space:]]+optional[[:space:]]+pam_gnome_keyring\\.so[[:space:]]+auto_start$' packaging/pam/focaldmd-fedora
    rg -q '^password[[:space:]]+optional[[:space:]]+pam_focald_secrets\\.so$' packaging/pam/focaldmd-fedora
    sudo install -Dm644 packaging/pam/focaldmd-fedora /etc/pam.d/focaldmd

install-session-target-fedora:
    sudo install -Dm644 packaging/systemd/user/focaldesk-session.target /usr/lib/systemd/user/focaldesk-session.target
    # Migrate services formerly enabled directly under the shared desktop target.
    rm -f "$HOME/.config/systemd/user/graphical-session.target.wants/"focaldesk-*.service
    rm -f "$HOME/.config/systemd/user/graphical-session.target.wants/focal-launchd.service"
    rm -f "$HOME/.config/systemd/user/focaldesk-session.target.wants/focaldesk-polkitd.service"

install-power-service-fedora:
    cargo build --release -p focaldesk-powerd
    sudo install -Dm755 target/release/focaldesk-powerd /usr/bin/focaldesk-powerd
    sudo install -Dm644 packaging/systemd/user/focaldesk-powerd-fedora.service /usr/lib/systemd/user/focaldesk-powerd.service
    # One-time migration: focaldesk-powerd.service moved from default.target to graphical-session.target.
    rm -f "$HOME/.config/systemd/user/default.target.wants/focaldesk-powerd.service"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-powerd.service || echo "Skipping systemd user enable: no user bus available"

install-notifications-service-fedora:
    cargo build --release -p focaldesk-notificationsd
    sudo install -Dm755 target/release/focaldesk-notificationsd /usr/bin/focaldesk-notificationsd
    sudo install -Dm644 packaging/systemd/user/focaldesk-notificationsd-fedora.service /usr/lib/systemd/user/focaldesk-notificationsd.service
    # One-time migration: focaldesk-notificationsd.service moved from default.target to graphical-session.target.
    rm -f "$HOME/.config/systemd/user/default.target.wants/focaldesk-notificationsd.service"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-notificationsd.service || echo "Skipping systemd user enable: no user bus available"

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
