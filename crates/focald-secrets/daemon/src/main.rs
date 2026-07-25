//! focald-secrets — Focaldesk credential broker.
//!
//! Two surfaces over one encrypted store:
//!   * native focaldm-ipc socket with per-client ACLs (Focaldesk daemons)
//!   * org.freedesktop.secrets on the session bus (third-party apps: libsecret,
//!     nm-applet, browsers, oo7, python-secretstorage, ...)

use focald_secrets::{acl, dbus, ipc, shared, store};
use std::path::PathBuf;

fn memory_lock_required() -> bool {
    std::env::var_os("FOCALD_SECRETS_REQUIRE_MLOCK").as_deref() == Some(std::ffi::OsStr::new("1"))
}

fn lock_memory(flags: libc::c_int, stage: &str) -> std::io::Result<()> {
    // SAFETY: mlockall has no memory-safety preconditions.
    if unsafe { libc::mlockall(flags) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if memory_lock_required() {
        Err(std::io::Error::other(format!(
            "mlockall failed {stage}: {error}; refusing to run with swappable secrets"
        )))
    } else {
        log::warn!(
            "mlockall failed {stage} ({error}); secrets may be swappable — \
             raise LimitMEMLOCK or use encrypted swap/zram"
        );
        Ok(())
    }
}

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

fn legacy_data_file() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".local/share/focaldesk/secrets.db"))
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

    // A credential daemon must not be inspectable or dumped by ordinary
    // same-uid processes. Do this before loading the master key or database.
    // SAFETY: prctl(PR_SET_DUMPABLE) accepts an integer flag and no pointer.
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // Pin all pages currently mapped before any key material is loaded. We do
    // not request MCL_FUTURE yet because zbus still needs to create its worker
    // thread and allocate its stack.
    lock_memory(libc::MCL_CURRENT, "before loading the master key")?;

    let mut key = store::load_master_key()?;
    let db_path = data_file()?;
    if std::env::var_os("STATE_DIRECTORY").is_some() {
        if let Some(legacy_path) = legacy_data_file() {
            if legacy_path != db_path && store::migrate_legacy_store(&legacy_path, &db_path)? {
                log::info!(
                    "store: migrated encrypted database from {} to {}; source retained",
                    legacy_path.display(),
                    db_path.display()
                );
            }
        }
    }
    let store = store::Store::open(&db_path, &key)?;
    // Store loading allocates and decrypts item buffers. Pin those pages
    // immediately, closing the startup window before any IPC surface exists.
    lock_memory(libc::MCL_CURRENT, "after opening the encrypted store")?;
    if store::consume_runtime_master_key()? {
        log::warn!("consumed explicitly enabled legacy master-key handoff");
    }
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
    // Development sessions warn on failure. The packaged production unit sets
    // FOCALD_SECRETS_REQUIRE_MLOCK=1, making any failure fatal.
    lock_memory(
        libc::MCL_CURRENT | libc::MCL_FUTURE,
        "while enabling future-page locking",
    )?;

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
