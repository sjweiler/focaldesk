//! focald-secrets-keytool — manage the wrapped master key without PAM.
//!
//!   keytool init      create wrapped key (+ provision runtime key)
//!   keytool unlock    unwrap and write the runtime key for this session
//!   keytool rewrap    change the wrapping password
//!   keytool status    show key file states
//!
//! Passwords are read from the terminal with echo off, or from stdin when
//! piped (for scripted tests). This is the manual counterpart of the PAM
//! module; a session set up by either is identical to the daemon.

use std::io::{BufRead, IsTerminal, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use zeroize::Zeroizing;

fn wrapped_path() -> PathBuf {
    std::env::var_os("FOCALD_SECRETS_WRAPPED")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".local/share/focaldesk/secrets.key.enc"))
        })
        .expect("HOME not set")
}

fn runtime_path() -> PathBuf {
    std::env::var_os("FOCALD_SECRETS_KEYFILE")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_RUNTIME_DIR")
                .map(|r| PathBuf::from(r).join("focaldesk/secrets.key"))
        })
        .expect("XDG_RUNTIME_DIR not set")
}

fn prompt_password(prompt: &str) -> Zeroizing<Vec<u8>> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        let mut line = String::new();
        stdin.lock().read_line(&mut line).expect("read password");
        let pw = Zeroizing::new(line.trim_end_matches('\n').as_bytes().to_vec());
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
    Zeroizing::new(line.trim_end_matches('\n').as_bytes().to_vec())
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
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

fn main() {
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
            write_600(&runtime_path(), master.as_ref()).expect("write runtime key");
            eprintln!("initialized {} and unlocked for this session", wp.display());
        }
        "unlock" => {
            let wrapped = std::fs::read(&wp).unwrap_or_else(|e| {
                eprintln!("{}: {e} (run `init` first)", wp.display());
                std::process::exit(1);
            });
            let pw = prompt_password("Keyring password: ");
            match keywrap::unwrap(&pw, &wrapped) {
                Ok(master) => {
                    write_600(&runtime_path(), master.as_ref()).expect("write runtime key");
                    eprintln!("unlocked");
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
        "rewrap" => {
            let wrapped = std::fs::read(&wp).expect("read wrapped key");
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
            let rt = runtime_path();
            println!(
                "runtime key: {} ({})",
                rt.display(),
                if rt.exists() {
                    "present — session unlocked"
                } else {
                    "absent — locked"
                }
            );
        }
        _ => {
            eprintln!("usage: focald-secrets-keytool init|unlock|rewrap|status");
            std::process::exit(2);
        }
    }
}
