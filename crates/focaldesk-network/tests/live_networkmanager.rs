//! Exercises the real NetworkManager backend against the live system bus.
//! Requires NetworkManager to actually be running, so it's `#[ignore]`d:
//! `cargo test -p focaldesk-network --test live_networkmanager -- --ignored --nocapture`.

use focaldesk_network::auto_backend;

#[tokio::test]
#[ignore = "requires a live NetworkManager on the system bus"]
async fn reports_a_plausible_primary_connection() {
    let backend = auto_backend().await.expect("backend should initialize");
    println!("backend: {}", backend.name());

    let state = backend
        .current_state()
        .await
        .expect("current_state should succeed");
    println!("{state:#?}");

    assert!(
        state.interface_name.is_some(),
        "expected a primary interface name to be resolved"
    );
}
