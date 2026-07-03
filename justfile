desktop_bin := "/usr/local/bin/focaldesk-desktop"

build:
    cargo build

release-desktop:
    cargo build --release -p focaldesk-desktop

release-server:
    cargo build --release -p focaldesk-server

release-launchd:
    cargo build --release -p focal-launchd

release-portal:
    cargo build --release -p focaldesk-portal

install-server-service:
    cargo build --release -p focaldesk-server
    install -Dm755 target/release/focaldesk-server "$HOME/.local/bin/focaldesk-server"
    install -Dm644 packaging/systemd/user/focaldesk-server.service "$HOME/.config/systemd/user/focaldesk-server.service"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-server.service || echo "Skipping systemd user enable: no user bus available"

install-launch-service:
    cargo build --release -p focal-launchd
    install -Dm755 target/release/focal-launchd "$HOME/.local/bin/focal-launchd"
    install -Dm644 packaging/systemd/user/focal-launchd.service "$HOME/.config/systemd/user/focal-launchd.service"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focal-launchd.service || echo "Skipping systemd user enable: no user bus available"

install-portal:
    cargo build --release -p focaldesk-portal
    install -Dm755 target/release/focaldesk-portal "$HOME/.local/bin/focaldesk-portal"
    mkdir -p "$HOME/.config/xdg-desktop-portal-wlr"
    target/release/focaldesk-portal --print-xdpw-config > "$HOME/.config/xdg-desktop-portal-wlr/config"
    systemctl --user restart xdg-desktop-portal xdg-desktop-portal-wlr || echo "Restart portal services manually after logging into FocalDesk"

install-files:
    cargo build --release -p focaldesk-files
    sudo install -Dm755 target/release/focaldesk-files /usr/local/bin/focaldesk-files

install-settings:
    cargo build --release -p focaldesk-settings
    sudo install -Dm755 target/release/focaldesk-settings /usr/local/bin/focaldesk-settings

install-ai-console:
    cargo build --release -p focaldesk-ai-console
    sudo install -Dm755 target/release/focaldesk-ai-console /usr/local/bin/focaldesk-ai-console

install-desktop:
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

install-server-service-fedora:
    cargo build --release -p focaldesk-server
    sudo install -Dm755 target/release/focaldesk-server /usr/bin/focaldesk-server
    sudo install -Dm644 packaging/systemd/user/focaldesk-server-fedora.service /usr/lib/systemd/user/focaldesk-server.service
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-server.service || echo "Skipping systemd user enable: no user bus available"

install-launch-service-fedora:
    cargo build --release -p focal-launchd
    sudo install -Dm755 target/release/focal-launchd /usr/bin/focal-launchd
    sudo install -Dm644 packaging/systemd/user/focal-launchd-fedora.service /usr/lib/systemd/user/focal-launchd.service
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focal-launchd.service || echo "Skipping systemd user enable: no user bus available"

run:
    cargo run

fmt:
    cargo fmt

lint:
    cargo clippy -- -D warnings

test:
    cargo test
