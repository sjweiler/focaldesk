//! pam_focald_secrets — provision the focald-secrets master key at login.
//!
//! Mirrors the pam_gnome_keyring flow:
//!   auth      optional pam_focald_secrets.so   (stashes a copy of the authtok)
//!   session   optional pam_focald_secrets.so   (unwraps and starts broker)
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
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
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
            // `pam_sm_authenticate` gives PAM a CString::into_raw pointer.
            // Reclaim the same allocation, expose its complete capacity as a
            // Vec, and scrub the password plus terminator before freeing it.
            let mut bytes = CString::from_raw(data as *mut c_char).into_bytes_with_nul();
            bytes.zeroize();
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
    let parent_path = path
        .parent()
        .ok_or_else(|| std::io::Error::other("credential path has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("credential path has no filename"))?;
    let parent = open_directory_chain(parent_path, u, true)?;
    let name = CString::new(name.as_bytes()).map_err(std::io::Error::other)?;

    // All privileged mutations happen through the descriptor returned by
    // openat(). In particular, never chown(2) a pathname in a user-writable
    // directory: the user could replace that pathname with a symlink between
    // the open and chown and make root change ownership of an arbitrary file.
    let mut random = [0u8; 8];
    if unsafe { libc::getrandom(random.as_mut_ptr() as *mut c_void, random.len(), 0) }
        != random.len() as isize
    {
        return Err(std::io::Error::last_os_error());
    }
    let temporary = CString::new(format!(
        ".focald-secrets.tmp.{:016x}",
        u64::from_ne_bytes(random)
    ))
    .unwrap();
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            temporary.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            mode as libc::mode_t,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    let result = (|| {
        if unsafe { libc::fchown(file.as_raw_fd(), u.uid, u.gid) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if unsafe { libc::fchmod(file.as_raw_fd(), mode as libc::mode_t) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        file.write_all(data)?;
        file.sync_all()?;
        if unsafe {
            libc::renameat(
                parent.as_raw_fd(),
                temporary.as_ptr(),
                parent.as_raw_fd(),
                name.as_ptr(),
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        parent.sync_all()
    })();
    if result.is_err() {
        unsafe {
            libc::unlinkat(parent.as_raw_fd(), temporary.as_ptr(), 0);
        }
    }
    result
}

fn read_owned(
    path: &Path,
    minimum_len: usize,
    maximum_len: usize,
    u: &UserInfo,
) -> std::io::Result<Zeroizing<Vec<u8>>> {
    let parent_path = path
        .parent()
        .ok_or_else(|| std::io::Error::other("credential path has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("credential path has no filename"))?;
    let parent = open_directory_chain(parent_path, u, false)?;
    let name = CString::new(name.as_bytes()).map_err(std::io::Error::other)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != u.uid
        || metadata.mode() & 0o077 != 0
        || metadata.len() < minimum_len as u64
        || metadata.len() > maximum_len as u64
    {
        return Err(std::io::Error::other(format!(
            "unsafe ownership, permissions, type, or size for {}",
            path.display()
        )));
    }
    let expected_len = metadata.len() as usize;
    let mut data = Zeroizing::new(Vec::with_capacity(expected_len));
    std::io::Read::by_ref(&mut file)
        .take(expected_len as u64 + 1)
        .read_to_end(&mut data)?;
    if data.len() != expected_len {
        return Err(std::io::Error::other(format!(
            "{} changed size while being read",
            path.display()
        )));
    }
    Ok(data)
}

fn open_root_directory() -> std::io::Result<std::fs::File> {
    let root = c"/";
    let fd = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

fn open_directory_chain(path: &Path, u: &UserInfo, create: bool) -> std::io::Result<std::fs::File> {
    if !path.is_absolute() {
        return Err(std::io::Error::other(
            "credential directory is not absolute",
        ));
    }
    let mut current = open_root_directory()?;
    let mut traversed = PathBuf::from("/");
    let components: Vec<_> = path.components().collect();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(component) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err(std::io::Error::other("unsafe credential path component"));
        };
        traversed.push(component);
        let component = CString::new(component.as_bytes()).map_err(std::io::Error::other)?;
        let mut created = false;
        let mut fd = unsafe {
            libc::openat(
                current.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0
            && create
            && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound
        {
            if unsafe { libc::mkdirat(current.as_raw_fd(), component.as_ptr(), 0o700) } != 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(std::io::Error::last_os_error());
            }
            fd = unsafe {
                libc::openat(
                    current.as_raw_fd(),
                    component.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
            created = true;
        }
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let next = unsafe { std::fs::File::from_raw_fd(fd) };
        if created
            && (unsafe { libc::fchown(next.as_raw_fd(), u.uid, u.gid) } != 0
                || unsafe { libc::fchmod(next.as_raw_fd(), 0o700) } != 0)
        {
            return Err(std::io::Error::last_os_error());
        }
        let metadata = next.metadata()?;
        let mode = metadata.mode();
        if mode & 0o002 != 0 || (mode & 0o020 != 0 && metadata.uid() != u.uid) {
            return Err(std::io::Error::other(format!(
                "insecure permissions on focald-secrets path component: {}",
                traversed.display()
            )));
        }
        // The final secrets directory is private to the target user. Perform
        // ownership and mode changes only through its already-verified fd.
        if create
            && index + 1 == components.len()
            && (unsafe { libc::fchown(next.as_raw_fd(), u.uid, u.gid) } != 0
                || unsafe { libc::fchmod(next.as_raw_fd(), 0o700) } != 0)
        {
            return Err(std::io::Error::last_os_error());
        }
        current = next;
    }
    Ok(current)
}

fn system_credential_path(uid: libc::uid_t) -> PathBuf {
    PathBuf::from(format!("/run/focald-secrets/{uid}/master"))
}

fn write_system_credential(
    path: &Path,
    data: &[u8],
    expected_owner: libc::uid_t,
) -> std::io::Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "the PAM session hook must run as root",
        ));
    }
    let parent_path = path
        .parent()
        .ok_or_else(|| std::io::Error::other("credential path has no parent"))?;
    let parent = open_root_owned_directory_chain(parent_path, true, expected_owner)?;
    let name = CString::new(
        path.file_name()
            .ok_or_else(|| std::io::Error::other("credential path has no filename"))?
            .as_bytes(),
    )
    .map_err(std::io::Error::other)?;
    let mut random = [0u8; 8];
    if unsafe { libc::getrandom(random.as_mut_ptr() as *mut c_void, random.len(), 0) }
        != random.len() as isize
    {
        return Err(std::io::Error::last_os_error());
    }
    let temporary =
        CString::new(format!(".master.tmp.{:016x}", u64::from_ne_bytes(random))).unwrap();
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            temporary.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o400,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    let result = (|| {
        if unsafe { libc::fchown(file.as_raw_fd(), 0, 0) } != 0
            || unsafe { libc::fchmod(file.as_raw_fd(), 0o400) } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        file.write_all(data)?;
        file.sync_all()?;
        if unsafe {
            libc::renameat(
                parent.as_raw_fd(),
                temporary.as_ptr(),
                parent.as_raw_fd(),
                name.as_ptr(),
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        parent.sync_all()
    })();
    if result.is_err() {
        unsafe {
            libc::unlinkat(parent.as_raw_fd(), temporary.as_ptr(), 0);
        }
    }
    result
}

fn open_root_owned_directory_chain(
    path: &Path,
    create: bool,
    expected_owner: libc::uid_t,
) -> std::io::Result<std::fs::File> {
    if !path.is_absolute() {
        return Err(std::io::Error::other(
            "credential directory is not absolute",
        ));
    }
    let mut current = open_root_directory()?;
    let mut traversed = PathBuf::from("/");
    let components: Vec<_> = path.components().collect();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(component) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err(std::io::Error::other("unsafe credential path component"));
        };
        traversed.push(component);
        let component = CString::new(component.as_bytes()).map_err(std::io::Error::other)?;
        let mut created = false;
        let mut fd = unsafe {
            libc::openat(
                current.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        let final_component = index + 1 == components.len();
        if fd < 0
            && create
            && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound
        {
            let private_component = final_component || traversed.starts_with("/run/focald-secrets");
            let mode = if private_component { 0o700 } else { 0o755 };
            if unsafe { libc::mkdirat(current.as_raw_fd(), component.as_ptr(), mode) } != 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists
            {
                return Err(std::io::Error::last_os_error());
            }
            fd = unsafe {
                libc::openat(
                    current.as_raw_fd(),
                    component.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
            created = true;
        }
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let next = unsafe { std::fs::File::from_raw_fd(fd) };
        if created
            && traversed.starts_with("/run/focald-secrets")
            && (unsafe { libc::fchown(next.as_raw_fd(), expected_owner, 0) } != 0
                || unsafe { libc::fchmod(next.as_raw_fd(), 0o700) } != 0)
        {
            return Err(std::io::Error::last_os_error());
        }
        let metadata = next.metadata()?;
        let under_credential_root = traversed.starts_with("/run/focald-secrets");
        if under_credential_root
            && (metadata.uid() != expected_owner
                || metadata.gid() != 0
                || metadata.mode() & 0o077 != 0)
        {
            return Err(std::io::Error::other(format!(
                "{} must be root-owned and inaccessible to group/other",
                traversed.display()
            )));
        }
        current = next;
    }
    Ok(current)
}

fn remove_system_credential(path: &Path, expected_owner: libc::uid_t) -> std::io::Result<()> {
    let parent = open_root_owned_directory_chain(
        path.parent()
            .ok_or_else(|| std::io::Error::other("credential path has no parent"))?,
        false,
        expected_owner,
    )?;
    let name = CString::new(
        path.file_name()
            .ok_or_else(|| std::io::Error::other("credential path has no filename"))?
            .as_bytes(),
    )
    .map_err(std::io::Error::other)?;
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } != 0
        && std::io::Error::last_os_error().kind() != std::io::ErrorKind::NotFound
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn flock_exclusive_with_timeout(
    file: &std::fs::File,
    timeout: std::time::Duration,
) -> std::io::Result<()> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EWOULDBLOCK) {
            return Err(error);
        }
        if std::time::Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out waiting for another focald-secrets PAM operation",
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn acquire_provision_lock(uid: libc::uid_t) -> std::io::Result<std::fs::File> {
    if unsafe { libc::geteuid() } != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "the PAM provisioning lock requires root",
        ));
    }
    let directory_path = PathBuf::from(format!("/run/focald-secrets/{uid}"));
    let directory = open_root_owned_directory_chain(&directory_path, true, 0)?;
    let name = c"provision.lock";
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    if unsafe { libc::fchown(file.as_raw_fd(), 0, 0) } != 0
        || unsafe { libc::fchmod(file.as_raw_fd(), 0o600) } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.gid() != 0
        || metadata.mode() & 0o077 != 0
    {
        return Err(std::io::Error::other(
            "unsafe focald-secrets PAM provisioning lock",
        ));
    }
    flock_exclusive_with_timeout(&file, std::time::Duration::from_secs(30))?;
    Ok(file)
}

fn encrypted_store_exists(user: &UserInfo) -> bool {
    [
        PathBuf::from(format!("/var/lib/focald-secrets/{}/secrets.db", user.uid)),
        user.home.join(".local/share/focaldesk/secrets.db"),
    ]
    .iter()
    .any(|path| std::fs::symlink_metadata(path).is_ok())
}

fn start_system_broker(uid: libc::uid_t) -> std::io::Result<()> {
    let unit = format!("focald-secrets@{uid}.service");
    let mut child = Command::new("/usr/bin/systemctl")
        .args([
            "--system",
            "--no-ask-password",
            "--quiet",
            "start",
            unit.as_str(),
        ])
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("timed out starting {unit}"),
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "systemctl failed to start {unit}: {status}"
        )))
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
        // PAM owns this allocation after pam_set_data succeeds and returns it
        // through pam_get_data until cleanup. PAM passwords originate as C
        // strings, so the captured token cannot contain an interior NUL.
        let Ok(stash) = CString::new(tok.as_slice()) else {
            syslog("authtok unexpectedly contains NUL");
            return PAM_IGNORE;
        };
        let raw = stash.into_raw();
        let rc = unsafe {
            pam_set_data(
                pamh,
                STASH_NAME.as_ptr() as *const c_char,
                raw as *mut c_void,
                Some(stash_cleanup),
            )
        };
        if rc != PAM_SUCCESS {
            unsafe {
                let mut bytes = CString::from_raw(raw).into_bytes_with_nul();
                bytes.zeroize();
            }
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
            // The pointer is the CString allocation registered above. PAM
            // guarantees module data remains valid until its cleanup callback.
            let stash = unsafe { CStr::from_ptr(data as *const c_char) };
            Some(Zeroizing::new(stash.to_bytes().to_vec()))
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
    if user.uid == 0 {
        syslog("refusing to start a desktop credential broker for uid 0");
        return PAM_SUCCESS;
    }
    let _provision_lock = match acquire_provision_lock(user.uid) {
        Ok(lock) => lock,
        Err(error) => {
            syslog(&format!(
                "cannot serialize credential provisioning for uid {}: {error}",
                user.uid
            ));
            return PAM_SUCCESS;
        }
    };

    let wrapped_path = wrapped_key_path(&user.home);
    let master = match read_owned(
        &wrapped_path,
        keywrap::MIN_WRAPPED_LEN,
        keywrap::MAX_WRAPPED_LEN,
        &user,
    ) {
        Ok(wrapped) => match keywrap::unwrap(&password, &wrapped) {
            Ok(m) => {
                if keywrap::needs_upgrade(&wrapped) {
                    match keywrap::wrap(&password, &m) {
                        Ok(upgraded) => {
                            if let Err(e) = write_owned(&wrapped_path, &upgraded, 0o600, &user) {
                                syslog(&format!(
                                    "cannot upgrade {} to FKEY2: {e}",
                                    wrapped_path.display()
                                ));
                            } else {
                                syslog("upgraded wrapped master key to FKEY2/Argon2id");
                            }
                        }
                        Err(e) => syslog(&format!("cannot create FKEY2 upgrade: {e}")),
                    }
                }
                m
            }
            Err(e) => {
                syslog(&format!("unwrap failed ({e}); keyring not unlocked"));
                return PAM_SUCCESS;
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // First login: create and wrap a fresh master key.
            if encrypted_store_exists(&user) {
                syslog(&format!(
                    "{} is missing but an encrypted secrets database already exists; refusing to generate a replacement key",
                    wrapped_path.display()
                ));
                return PAM_SUCCESS;
            }
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

    let credential = system_credential_path(user.uid);
    if let Err(e) = write_system_credential(&credential, master.as_ref(), 0) {
        syslog(&format!(
            "cannot stage root-only service credential {}: {e}",
            credential.display()
        ));
        return PAM_SUCCESS;
    }
    let start_result = start_system_broker(user.uid);
    if let Err(e) = remove_system_credential(&credential, 0) {
        syslog(&format!(
            "cannot remove staged service credential {}: {e}",
            credential.display()
        ));
    }
    if let Err(e) = start_result {
        syslog(&format!("cannot start credential broker: {e}"));
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
    let _provision_lock = match acquire_provision_lock(user.uid) {
        Ok(lock) => lock,
        Err(error) => {
            syslog(&format!(
                "cannot serialize password rewrap for uid {}: {error}",
                user.uid
            ));
            return PAM_IGNORE;
        }
    };
    let path = wrapped_key_path(&user.home);
    let Ok(mut wrapped) = read_owned(
        &path,
        keywrap::MIN_WRAPPED_LEN,
        keywrap::MAX_WRAPPED_LEN,
        &user,
    ) else {
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

#[cfg(test)]
mod tests {
    use super::{flock_exclusive_with_timeout, read_owned, write_owned, UserInfo};
    use std::fs::OpenOptions;
    use std::os::unix::fs::{symlink, MetadataExt};

    fn current_user() -> UserInfo {
        // SAFETY: the identity syscalls have no preconditions.
        UserInfo {
            uid: unsafe { libc::geteuid() },
            gid: unsafe { libc::getegid() },
            home: std::path::PathBuf::new(),
        }
    }

    #[test]
    fn descriptor_based_write_roundtrips() {
        let workspace = std::env::current_dir().unwrap();
        let directory = tempfile::Builder::new()
            .prefix("pam-focald-secrets-")
            .tempdir_in(workspace)
            .unwrap();
        let path = directory.path().join("private/key");
        let user = current_user();

        write_owned(&path, b"secret", 0o600, &user).unwrap();
        let value = read_owned(&path, 6, 6, &user).unwrap();
        assert_eq!(value.as_slice(), b"secret");
        let metadata = path.metadata().unwrap();
        assert_eq!(metadata.uid(), user.uid);
        assert_eq!(metadata.mode() & 0o777, 0o600);
    }

    #[test]
    fn read_rejects_symlink() {
        let workspace = std::env::current_dir().unwrap();
        let directory = tempfile::Builder::new()
            .prefix("pam-focald-secrets-")
            .tempdir_in(workspace)
            .unwrap();
        let path = directory.path().join("wrapped");
        symlink("/etc/passwd", &path).unwrap();
        assert!(read_owned(
            &path,
            keywrap::MIN_WRAPPED_LEN,
            keywrap::MAX_WRAPPED_LEN,
            &current_user()
        )
        .is_err());
    }

    #[test]
    fn provisioning_lock_serializes_callers() {
        let workspace = std::env::current_dir().unwrap();
        let directory = tempfile::Builder::new()
            .prefix("pam-focald-secrets-lock-")
            .tempdir_in(workspace)
            .unwrap();
        let path = directory.path().join("lock");
        let first = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let second = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        flock_exclusive_with_timeout(&first, std::time::Duration::from_millis(50)).unwrap();
        let error = flock_exclusive_with_timeout(&second, std::time::Duration::from_millis(50))
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        drop(first);
        flock_exclusive_with_timeout(&second, std::time::Duration::from_millis(50)).unwrap();
    }
}
