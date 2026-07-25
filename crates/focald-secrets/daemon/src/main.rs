//! focald-secrets — Focaldesk credential broker.
//!
//! Two surfaces over one encrypted store:
//!   * native focaldm-ipc socket with per-client ACLs (Focaldesk daemons)
//!   * org.freedesktop.secrets on the session bus (third-party apps: libsecret,
//!     nm-applet, browsers, oo7, python-secretstorage, ...)

mod acl;
mod dbus;
mod ipc;
mod shared;
mod sscrypto;
mod store;

use std::path::PathBuf;

fn data_file() -> std::io::Result<PathBuf> {
    let base = std::env::var_os("FOCALD_SECRETS_DB")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_DATA_HOME").map(|d| PathBuf::from(d).join("focaldesk/secrets.db"))
        })
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".local/share/focaldesk/secrets.db"))
        })
        .ok_or_else(|| std::io::Error::other("cannot determine data path (HOME unset)"))?;
    Ok(base)
}

fn acl_file() -> PathBuf {
    std::env::var_os("FOCALD_SECRETS_ACL")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(|d| PathBuf::from(d).join("focaldesk/secrets-acl.toml"))
        })
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".config/focaldesk/secrets-acl.toml"))
        })
        .unwrap_or_else(|| PathBuf::from("/etc/focaldesk/secrets-acl.toml"))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let mut key = store::load_master_key()?;
    let db_path = data_file()?;
    let store = store::Store::open(&db_path, &key)?;
    zeroize::Zeroize::zeroize(&mut key);
    log::info!(
        "store: {} ({} item(s))",
        db_path.display(),
        store.items().len()
    );
    let acl = acl::Acl::new(acl_file());
    let shared = shared::Shared::new(store, acl);

    // D-Bus surface. Failure (e.g. gnome-keyring already owns the name) is not
    // fatal — the native surface keeps working and we log loudly.
    match dbus::start(shared.clone()).await {
        Ok(conn) => {
            *shared.dbus.lock().await = Some(conn);
        }
        Err(e) => {
            log::error!(
                "dbus: could not claim org.freedesktop.secrets ({e}); \
                 is another secret service (gnome-keyring?) running? \
                 continuing with native IPC only"
            );
            // Still connect to the bus if possible, for systemd peer lookup.
            if let Ok(conn) = zbus::Connection::session().await {
                *shared.dbus.lock().await = Some(conn);
            }
        }
    }

    // zbus lazily creates its executor worker while establishing the service
    // connection above. Pin memory only after that thread exists: MCL_FUTURE
    // otherwise makes pthread_create fail with EAGAIN when the new stack would
    // exceed RLIMIT_MEMLOCK, preventing the broker from starting at all.
    // Failure remains a warning so low limits never make credentials
    // unavailable.
    // SAFETY: mlockall has no memory-safety preconditions.
    if unsafe { libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE) } != 0 {
        log::warn!(
            "mlockall failed ({}); secrets may be swappable — raise LimitMEMLOCK or use encrypted swap/zram",
            std::io::Error::last_os_error()
        );
    }

    // Native IPC surface.
    let listener = ipc::make_listener()?;
    let ipc_shared = shared.clone();
    tokio::spawn(async move { ipc::serve(listener, ipc_shared).await });

    // Run until SIGTERM/SIGINT, then flush.
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => log::info!("SIGINT"),
        _ = sigterm.recv() => log::info!("SIGTERM"),
    }
    if let Err(e) = shared.store.lock().await.save() {
        log::error!("final save failed: {e}");
    }
    Ok(())
}
