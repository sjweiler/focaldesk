//! focald-secrets-keytool — manage the wrapped master key without PAM.
//!
//!   keytool init      create a password-wrapped master key
//!   keytool unlock    unwrap into an explicit development key file
//!   keytool rewrap    change the wrapping password
//!   keytool status    show key file states
//!
//! Passwords are read from the terminal with echo off, or from stdin when
//! piped (for scripted tests). This is the manual counterpart of the PAM
//! module. Production login uses the PAM/systemd credential path; manual
//! unlock intentionally requires FOCALD_SECRETS_KEYFILE so it cannot silently
//! recreate the old same-UID-readable handoff.

use std::io::{BufRead, IsTerminal, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use zeroize::{Zeroize, Zeroizing};

fn wrapped_path() -> PathBuf {
    std::env::var_os("FOCALD_SECRETS_WRAPPED")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".local/share/focaldesk/secrets.key.enc"))
        })
        .expect("HOME not set")
}

fn development_key_path() -> Option<PathBuf> {
    std::env::var_os("FOCALD_SECRETS_KEYFILE").map(PathBuf::from)
}

fn prompt_password(prompt: &str) -> Zeroizing<Vec<u8>> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        let mut line = String::new();
        stdin.lock().read_line(&mut line).expect("read password");
        let pw = Zeroizing::new(line.trim_end_matches('\n').as_bytes().to_vec());
        line.zeroize();
        return pw;
    }
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    // Echo off via termios.
    let fd = 0;
    let mut term: libc::termios = unsafe { std::mem::zeroed() };
    unsafe { libc::tcgetattr(fd, &mut term) };
    let saved = term;
    term.c_lflag &= !libc::ECHO;
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &term) };
    let mut line = String::new();
    let r = stdin.lock().read_line(&mut line);
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &saved) };
    eprintln!();
    r.expect("read password");
    let password = Zeroizing::new(line.trim_end_matches('\n').as_bytes().to_vec());
    line.zeroize();
    password
}

fn ensure_dir_0700(dir: &std::path::Path) -> std::io::Result<()> {
    // Only impose 0700 on directories we create; never chmod a pre-existing
    // shared parent (e.g. /tmp) we may not own.
    if !dir.exists() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn write_600(path: &PathBuf, data: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        ensure_dir_0700(dir)?;
    }
    let tmp = path.with_extension(format!("tmp.{:016x}", rand::random::<u64>()));
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    if let Err(error) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn read_private(
    path: &PathBuf,
    minimum_len: usize,
    maximum_len: usize,
) -> std::io::Result<Zeroizing<Vec<u8>>> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    // SAFETY: geteuid has no preconditions.
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || metadata.len() < minimum_len as u64
        || metadata.len() > maximum_len as u64
    {
        return Err(std::io::Error::other(format!(
            "{} has unsafe ownership, permissions, type, or size",
            path.display()
        )));
    }
    let expected_len = metadata.len() as usize;
    let mut bytes = Zeroizing::new(Vec::with_capacity(expected_len));
    std::io::Read::take(&mut file, maximum_len as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() != expected_len {
        return Err(std::io::Error::other(
            "wrapped key changed while being read",
        ));
    }
    Ok(bytes)
}

fn main() {
    // SAFETY: prctl(PR_SET_DUMPABLE) accepts an integer flag and no pointer.
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
        eprintln!(
            "cannot disable process dumps: {}",
            std::io::Error::last_os_error()
        );
        std::process::exit(1);
    }
    let cmd = std::env::args().nth(1).unwrap_or_default();
    let wp = wrapped_path();
    match cmd.as_str() {
        "init" => {
            if wp.exists() {
                eprintln!(
                    "{} already exists; use rewrap to change password",
                    wp.display()
                );
                std::process::exit(1);
            }
            let pw = prompt_password("New keyring password: ");
            let (master, wrapped) = keywrap::create(&pw).expect("create");
            write_600(&wp, &wrapped).expect("write wrapped key");
            if let Some(path) = development_key_path() {
                write_600(&path, master.as_ref()).expect("write development key");
                eprintln!(
                    "initialized {} and wrote the explicit development key {}",
                    wp.display(),
                    path.display()
                );
            } else {
                eprintln!(
                    "initialized {}; the PAM session hook will unlock it at login",
                    wp.display()
                );
            }
        }
        "unlock" => {
            let wrapped = read_private(&wp, keywrap::MIN_WRAPPED_LEN, keywrap::MAX_WRAPPED_LEN)
                .unwrap_or_else(|e| {
                    eprintln!("{}: {e} (run `init` first)", wp.display());
                    std::process::exit(1);
                });
            let pw = prompt_password("Keyring password: ");
            match keywrap::unwrap(&pw, &wrapped) {
                Ok(master) => {
                    if keywrap::needs_upgrade(&wrapped) {
                        let upgraded = keywrap::wrap(&pw, &master).expect("upgrade to FKEY2");
                        write_600(&wp, &upgraded).expect("write FKEY2 upgrade");
                        eprintln!("upgraded wrapped key to FKEY2/Argon2id");
                    }
                    let Some(path) = development_key_path() else {
                        eprintln!(
                            "manual unlock is development/recovery-only; set \
                             FOCALD_SECRETS_KEYFILE to an explicit private path"
                        );
                        std::process::exit(1);
                    };
                    write_600(&path, master.as_ref()).expect("write development key");
                    eprintln!("wrote explicit development key {}", path.display());
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
        "rewrap" => {
            let wrapped = read_private(&wp, keywrap::MIN_WRAPPED_LEN, keywrap::MAX_WRAPPED_LEN)
                .expect("read wrapped key");
            let old = prompt_password("Current password: ");
            let new = prompt_password("New password: ");
            match keywrap::rewrap(&old, &new, &wrapped) {
                Ok(w) => {
                    write_600(&wp, &w).expect("write wrapped key");
                    eprintln!("rewrapped");
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
        "status" => {
            println!(
                "wrapped key: {} ({})",
                wp.display(),
                if wp.exists() { "present" } else { "absent" }
            );
            if let Some(path) = development_key_path() {
                println!(
                    "development key: {} ({})",
                    path.display(),
                    if path.exists() { "present" } else { "absent" }
                );
            } else {
                println!("development key: disabled (FOCALD_SECRETS_KEYFILE unset)");
            }
        }
        _ => {
            eprintln!("usage: focald-secrets-keytool init|unlock|rewrap|status");
            std::process::exit(2);
        }
    }
}
