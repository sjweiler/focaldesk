build:
    cargo build

release-server:
    cargo build --release -p focaldesk-server

release-portal:
    cargo build --release -p focaldesk-portal

install-server-service:
    cargo build --release -p focaldesk-server
    install -Dm755 target/release/focaldesk-server "$HOME/.local/bin/focaldesk-server"
    install -Dm644 packaging/systemd/user/focaldesk-server.service "$HOME/.config/systemd/user/focaldesk-server.service"
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-server.service || echo "Skipping systemd user enable: no user bus available"

install-portal:
    cargo build --release -p focaldesk-portal
    install -Dm755 target/release/focaldesk-portal "$HOME/.local/bin/focaldesk-portal"

install-desktop:
    cargo build --release -p focaldesk-desktop --no-default-features --features="drm xwayland"
    sudo install -Dm755 target/release/focaldesk-desktop /usr/bin/focaldesk-desktop

install-desktop-session:
    sudo install -Dm644 packaging/wayland-sessions/focaldesk.desktop /usr/share/wayland-sessions/focaldesk.desktop

install-server-service-fedora:
    cargo build --release -p focaldesk-server
    sudo install -Dm755 target/release/focaldesk-server /usr/bin/focaldesk-server
    sudo install -Dm644 packaging/systemd/user/focaldesk-server-fedora.service /usr/lib/systemd/user/focaldesk-server.service
    systemctl --user daemon-reload || echo "Skipping systemd user reload: no user bus available"
    systemctl --user enable --now focaldesk-server.service || echo "Skipping systemd user enable: no user bus available"

run:
    cargo run

fmt:
    cargo fmt

lint:
    cargo clippy -- -D warnings

test:
    cargo test
