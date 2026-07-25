//! pam_focald_secrets — provision the focald-secrets master key at login.
//!
//! Mirrors the pam_gnome_keyring flow:
//!   auth      optional pam_focald_secrets.so   (stashes a copy of the authtok)
//!   session   optional pam_focald_secrets.so   (unwraps and writes runtime key)
//!   password  optional pam_focald_secrets.so   (rewraps on password change)
//!
//! Place the session line AFTER pam_systemd.so so XDG_RUNTIME_DIR exists.
//! On first login the wrapped key file is created transparently.
//!
//! Failure policy: never blocks login. Session hooks always return
//! PAM_SUCCESS (errors are logged to syslog, never surfaced to the stack);
//! the auth hook returns PAM_IGNORE since it performs no authentication; the worst outcome of a bug here is a locked
//! keyring, never a locked-out user.

use libc::{c_char, c_int, c_void};
use std::ffi::{CStr, CString};
use std::io::Write;
use std::os::fd::FromRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use zeroize::{Zeroize, Zeroizing};

// ---- Linux-PAM ABI ---------------------------------------------------------

const PAM_SUCCESS: c_int = 0;
const PAM_IGNORE: c_int = 25;
const PAM_AUTHTOK: c_int = 6;
const PAM_OLDAUTHTOK: c_int = 7;
const PAM_UPDATE_AUTHTOK: c_int = 0x2000;

#[repr(C)]
pub struct PamHandle {
    _private: [u8; 0],
}

extern "C" {
    fn pam_get_item(pamh: *const PamHandle, item_type: c_int, item: *mut *const c_void) -> c_int;
    fn pam_get_user(
        pamh: *const PamHandle,
        user: *mut *const c_char,
        prompt: *const c_char,
    ) -> c_int;
    fn pam_set_data(
        pamh: *mut PamHandle,
        module_data_name: *const c_char,
        data: *mut c_void,
        cleanup: Option<extern "C" fn(*mut PamHandle, *mut c_void, c_int)>,
    ) -> c_int;
    fn pam_get_data(
        pamh: *const PamHandle,
        module_data_name: *const c_char,
        data: *mut *const c_void,
    ) -> c_int;
    fn pam_getenv(pamh: *mut PamHandle, name: *const c_char) -> *const c_char;
}

fn syslog(msg: &str) {
    // LOG_AUTHPRIV(10<<3) | LOG_WARNING(4)
    let tag = CString::new("pam_focald_secrets").unwrap();
    let m = CString::new(msg.replace('%', "%%")).unwrap_or_default();
    unsafe {
        libc::openlog(tag.as_ptr(), libc::LOG_PID, libc::LOG_AUTHPRIV);
        libc::syslog(libc::LOG_WARNING, m.as_ptr());
        libc::closelog();
    }
}

// ---- authtok stash ---------------------------------------------------------

const STASH_NAME: &[u8] = b"focald_secrets_authtok\0";

extern "C" fn stash_cleanup(_pamh: *mut PamHandle, data: *mut c_void, _status: c_int) {
    if !data.is_null() {
        unsafe {
            let boxed: Box<Zeroizing<Vec<u8>>> = Box::from_raw(data as *mut Zeroizing<Vec<u8>>);
            drop(boxed); // Zeroizing scrubs
        }
    }
}

fn get_authtok(pamh: *const PamHandle, which: c_int) -> Option<Zeroizing<Vec<u8>>> {
    let mut item: *const c_void = std::ptr::null();
    if unsafe { pam_get_item(pamh, which, &mut item) } != PAM_SUCCESS || item.is_null() {
        return None;
    }
    let s = unsafe { CStr::from_ptr(item as *const c_char) };
    Some(Zeroizing::new(s.to_bytes().to_vec()))
}

// ---- filesystem helpers (run as root during session setup) ----------------

struct UserInfo {
    uid: libc::uid_t,
    gid: libc::gid_t,
    home: PathBuf,
}

fn lookup_user(name: &CStr) -> Option<UserInfo> {
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0u8; 4096];
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwnam_r(
            name.as_ptr(),
            &mut pwd,
            buf.as_mut_ptr() as *mut c_char,
            buf.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() {
        return None;
    }
    let home = unsafe { CStr::from_ptr(pwd.pw_dir) };
    Some(UserInfo {
        uid: pwd.pw_uid,
        gid: pwd.pw_gid,
        home: PathBuf::from(home.to_string_lossy().into_owned()),
    })
}

fn wrapped_key_path(home: &Path) -> PathBuf {
    home.join(".local/share/focaldesk/secrets.key.enc")
}

fn write_owned(path: &Path, data: &[u8], mode: u32, u: &UserInfo) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.exists() {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            chown(dir, u)?;
        }
        validate_parent_chain(dir, u)?;
    }
    // Exclusive + no-follow prevents a user-created symlink from redirecting
    // this root-running PAM module into an arbitrary file.
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut f = open_exclusive_no_symlink(&tmp, mode)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    chown(&tmp, u)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

const RESOLVE_NO_SYMLINKS: u64 = 0x04;

fn open_exclusive_no_symlink(path: &Path, mode: u32) -> std::io::Result<std::fs::File> {
    let encoded =
        CString::new(path.as_os_str().as_encoded_bytes()).map_err(std::io::Error::other)?;
    let how = OpenHow {
        flags: (libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC) as u64,
        mode: mode as u64,
        resolve: RESOLVE_NO_SYMLINKS,
    };
    // openat2 resolves every path component without following symlinks, so a
    // user cannot swap a checked parent directory between validation and open.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            libc::AT_FDCWD,
            encoded.as_ptr(),
            &how,
            std::mem::size_of::<OpenHow>(),
        )
    } as libc::c_int;
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

fn validate_parent_chain(dir: &Path, u: &UserInfo) -> std::io::Result<()> {
    let mut current = Some(dir);
    while let Some(path) = current {
        let metadata = std::fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(std::io::Error::other(format!(
                "unsafe focald-secrets path component: {}",
                path.display()
            )));
        }
        let mode = metadata.mode();
        if mode & 0o002 != 0 || (mode & 0o020 != 0 && metadata.uid() != u.uid) {
            return Err(std::io::Error::other(format!(
                "insecure permissions on focald-secrets path component: {}",
                path.display()
            )));
        }
        current = path.parent();
    }
    Ok(())
}

fn chown(path: &Path, u: &UserInfo) -> std::io::Result<()> {
    let c = CString::new(path.as_os_str().as_encoded_bytes()).map_err(std::io::Error::other)?;
    if unsafe { libc::chown(c.as_ptr(), u.uid, u.gid) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn runtime_key_path(pamh: *mut PamHandle, uid: libc::uid_t) -> PathBuf {
    let name = CString::new("XDG_RUNTIME_DIR").unwrap();
    let v = unsafe { pam_getenv(pamh, name.as_ptr()) };
    let base = if v.is_null() {
        format!("/run/user/{uid}")
    } else {
        unsafe { CStr::from_ptr(v) }.to_string_lossy().into_owned()
    };
    let requested = PathBuf::from(&base);
    let expected = PathBuf::from(format!("/run/user/{uid}"));
    let valid = requested == expected
        && std::fs::symlink_metadata(&requested)
            .map(|m| m.is_dir() && m.uid() == uid && m.mode() & 0o077 == 0)
            .unwrap_or(false);
    if valid {
        requested.join("focaldesk/secrets.key")
    } else {
        expected.join("focaldesk/secrets.key")
    }
}

// ---- PAM entry points ------------------------------------------------------

/// auth: stash a copy of the authtok for the session phase (pam_unix has
/// already verified it by the time an `optional` module after it runs).
///
/// # Safety
///
/// `pamh` and the argument vector must be valid for the duration of the PAM
/// module call, as required by the Linux-PAM module ABI.
#[no_mangle]
pub unsafe extern "C" fn pam_sm_authenticate(
    pamh: *mut PamHandle,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    if let Some(tok) = get_authtok(pamh, PAM_AUTHTOK) {
        let boxed = Box::new(tok);
        let rc = unsafe {
            pam_set_data(
                pamh,
                STASH_NAME.as_ptr() as *const c_char,
                Box::into_raw(boxed) as *mut c_void,
                Some(stash_cleanup),
            )
        };
        if rc != PAM_SUCCESS {
            syslog("failed to stash authtok");
        }
    }
    PAM_IGNORE
}

/// # Safety
///
/// All pointers must satisfy the Linux-PAM module ABI.
#[no_mangle]
pub unsafe extern "C" fn pam_sm_setcred(
    _pamh: *mut PamHandle,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    PAM_SUCCESS
}

/// # Safety
///
/// `pamh` and the argument vector must be valid for the duration of the PAM
/// module call, as required by the Linux-PAM module ABI.
#[no_mangle]
pub unsafe extern "C" fn pam_sm_open_session(
    pamh: *mut PamHandle,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    // Recover the stashed authtok (or the live item, for su-style stacks).
    let tok: Option<Zeroizing<Vec<u8>>> = {
        let mut data: *const c_void = std::ptr::null();
        let rc = unsafe { pam_get_data(pamh, STASH_NAME.as_ptr() as *const c_char, &mut data) };
        if rc == PAM_SUCCESS && !data.is_null() {
            let stash = unsafe { &*(data as *const Zeroizing<Vec<u8>>) };
            Some(Zeroizing::new(stash.to_vec()))
        } else {
            get_authtok(pamh, PAM_AUTHTOK)
        }
    };
    let Some(password) = tok else {
        syslog("no authtok available (passwordless auth?); keyring not unlocked");
        return PAM_SUCCESS;
    };

    let mut user_ptr: *const c_char = std::ptr::null();
    if unsafe { pam_get_user(pamh, &mut user_ptr, std::ptr::null()) } != PAM_SUCCESS
        || user_ptr.is_null()
    {
        return PAM_SUCCESS;
    }
    let Some(user) = lookup_user(unsafe { CStr::from_ptr(user_ptr) }) else {
        return PAM_SUCCESS;
    };

    let wrapped_path = wrapped_key_path(&user.home);
    let master = match std::fs::read(&wrapped_path) {
        Ok(wrapped) => match keywrap::unwrap(&password, &wrapped) {
            Ok(m) => m,
            Err(e) => {
                syslog(&format!("unwrap failed ({e}); keyring not unlocked"));
                return PAM_SUCCESS;
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // First login: create and wrap a fresh master key.
            match keywrap::create(&password) {
                Ok((m, wrapped)) => {
                    if let Err(e) = write_owned(&wrapped_path, &wrapped, 0o600, &user) {
                        syslog(&format!("cannot write {}: {e}", wrapped_path.display()));
                        return PAM_SUCCESS;
                    }
                    syslog("initialized new wrapped master key");
                    m
                }
                Err(e) => {
                    syslog(&format!("key creation failed: {e}"));
                    return PAM_SUCCESS;
                }
            }
        }
        Err(e) => {
            syslog(&format!("cannot read {}: {e}", wrapped_path.display()));
            return PAM_SUCCESS;
        }
    };

    let rt = runtime_key_path(pamh, user.uid);
    if let Err(e) = write_owned(&rt, master.as_ref(), 0o600, &user) {
        syslog(&format!("cannot write runtime key {}: {e}", rt.display()));
    }
    PAM_SUCCESS
}

/// # Safety
///
/// All pointers must satisfy the Linux-PAM module ABI.
#[no_mangle]
pub unsafe extern "C" fn pam_sm_close_session(
    _pamh: *mut PamHandle,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    // Runtime dir is destroyed by logind on last-session logout; nothing to do.
    PAM_SUCCESS
}

/// password: rewrap the master key under the new login password.
///
/// # Safety
///
/// `pamh` and the argument vector must be valid for the duration of the PAM
/// module call, as required by the Linux-PAM module ABI.
#[no_mangle]
pub unsafe extern "C" fn pam_sm_chauthtok(
    pamh: *mut PamHandle,
    flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    if flags & PAM_UPDATE_AUTHTOK == 0 {
        return PAM_IGNORE;
    }
    let (Some(old), Some(new)) = (
        get_authtok(pamh, PAM_OLDAUTHTOK),
        get_authtok(pamh, PAM_AUTHTOK),
    ) else {
        return PAM_IGNORE;
    };
    let mut user_ptr: *const c_char = std::ptr::null();
    if unsafe { pam_get_user(pamh, &mut user_ptr, std::ptr::null()) } != PAM_SUCCESS
        || user_ptr.is_null()
    {
        return PAM_IGNORE;
    }
    let Some(user) = lookup_user(unsafe { CStr::from_ptr(user_ptr) }) else {
        return PAM_IGNORE;
    };
    let path = wrapped_key_path(&user.home);
    let Ok(mut wrapped) = std::fs::read(&path) else {
        return PAM_IGNORE; // no keyring yet; next login initializes with new pw
    };
    match keywrap::rewrap(&old, &new, &wrapped) {
        Ok(new_wrapped) => {
            if let Err(e) = write_owned(&path, &new_wrapped, 0o600, &user) {
                syslog(&format!("rewrap write failed: {e}"));
            } else {
                syslog("master key rewrapped for new password");
            }
        }
        Err(e) => syslog(&format!("rewrap failed ({e}); keyring keeps old password")),
    }
    wrapped.zeroize();
    PAM_SUCCESS
}
