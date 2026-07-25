//! CLI for previewing or importing credentials from GNOME Keyring.

use anyhow::{bail, Context, Result};
use focald_secrets::{import, store};
use zeroize::Zeroize as _;

#[derive(Default)]
struct Options {
    dry_run: bool,
    import: bool,
    force: bool,
    if_available: bool,
    no_activate: bool,
}

fn options() -> Result<Options> {
    let mut options = Options::default();
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--dry-run" => options.dry_run = true,
            "--import" => options.import = true,
            "--force" => options.force = true,
            "--if-available" => options.if_available = true,
            "--no-activate" => options.no_activate = true,
            "-h" | "--help" => {
                println!(
                    "usage: focald-secrets-import-gnome-keyring \\
                     (--dry-run|--import) [--force] [--if-available] [--no-activate]"
                );
                std::process::exit(0);
            }
            unknown => bail!("unknown option: {unknown}"),
        }
    }
    if options.dry_run == options.import {
        bail!("choose exactly one of --dry-run or --import");
    }
    Ok(options)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    // The importer temporarily holds every readable source credential.
    // SAFETY: prctl(PR_SET_DUMPABLE) accepts an integer flag and no pointer.
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error()).context("disable importer process dumps");
    }
    let options = options()?;
    let marker = import::marker_file()?;
    if options.import && marker.exists() && !options.force {
        println!(
            "GNOME Keyring migration already completed: {}",
            marker.display()
        );
        return Ok(());
    }
    if options.import {
        let key_file = store::master_key_path()?;
        if !key_file.exists() {
            if options.if_available {
                eprintln!(
                    "GNOME Keyring import deferred: Focaldesk key is not unlocked ({})",
                    key_file.display()
                );
                return Ok(());
            }
            bail!(
                "Focaldesk key is not unlocked at {}; log in through the PAM-managed Focaldesk session",
                key_file.display()
            );
        }
    }

    let connection = match zbus::Connection::session().await {
        Ok(connection) => connection,
        Err(error) if options.if_available => {
            eprintln!("GNOME Keyring import skipped: session bus unavailable ({error})");
            return Ok(());
        }
        Err(error) => return Err(error).context("connect to session bus"),
    };
    if let Err(error) = import::ensure_gnome_owner(&connection, !options.no_activate).await {
        if options.if_available {
            eprintln!("GNOME Keyring import skipped: {error:#}");
            return Ok(());
        }
        return Err(error);
    }

    let collected = import::collect(&connection, options.import).await?;
    if options.dry_run {
        println!(
            "GNOME Keyring: {} collection(s), {} readable item(s), {} locked, {} failed",
            collected.collections,
            collected.items.len(),
            collected.locked,
            collected.failed
        );
        for item in &collected.items {
            println!("  {} [{}]", item.label, item.item_type);
        }
        return Ok(());
    }

    let mut key = store::load_master_key().context("load Focaldesk master key")?;
    let mut store = store::Store::open(&import::data_file()?, &key)?;
    key.zeroize();
    let complete = collected.complete();
    let summary = import::apply(&mut store, collected)?;
    println!(
        "GNOME Keyring import: {} copied, {} already present, {} locked, {} failed",
        summary.imported, summary.skipped_existing, summary.locked, summary.failed
    );
    if complete {
        import::write_marker(&marker, &summary)?;
        println!("migration marker written: {}", marker.display());
    } else if !options.if_available {
        bail!("migration was partial; no completion marker was written");
    } else {
        eprintln!("GNOME Keyring migration was partial; it will retry next session");
    }
    Ok(())
}
