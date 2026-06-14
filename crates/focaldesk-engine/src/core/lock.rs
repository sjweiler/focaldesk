use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;
use std::time::{Duration, Instant};

pub const LOCK_PULSE_DURATION: Duration = Duration::from_millis(900);
const PAM_SUCCESS: c_int = 0;
const PAM_BUF_ERR: c_int = 5;
const PAM_CONV_ERR: c_int = 19;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockPulseKind {
    Accepted,
    Rejected,
}

#[derive(Clone, Copy, Debug)]
pub struct LockPulse {
    pub kind: LockPulseKind,
    pub started_at: Instant,
}

#[derive(Clone, Copy, Debug)]
pub struct LockPulseFrame {
    pub kind: LockPulseKind,
    pub elapsed: Duration,
}

#[derive(Clone, Debug)]
pub struct LockScreenState {
    pub active: bool,
    pub password: String,
    pub password_visible: bool,
    pub message: String,
    pub authenticating: bool,
    pub pulse: Option<LockPulse>,
}

#[derive(Clone, Debug)]
pub struct LockScreenSnapshot {
    pub active: bool,
    pub password_len: usize,
    pub password_visible: bool,
    pub password_text: String,
    pub message: String,
    pub authenticating: bool,
    pub pulse: Option<LockPulseFrame>,
}

impl LockScreenState {
    pub fn new() -> Self {
        Self {
            active: false,
            password: String::new(),
            password_visible: false,
            message: "Enter password".to_string(),
            authenticating: false,
            pulse: None,
        }
    }

    pub fn lock(&mut self) {
        self.active = true;
        self.password.clear();
        self.password_visible = false;
        self.message = "Enter password".to_string();
        self.authenticating = false;
        self.pulse = None;
    }

    pub fn unlock(&mut self) {
        self.active = false;
        self.password.clear();
        self.password_visible = false;
        self.message.clear();
        self.authenticating = false;
        self.pulse = None;
    }

    pub fn push_char(&mut self, ch: char) {
        self.password.push(ch);
        self.message = "Enter password".to_string();
    }

    pub fn backspace(&mut self) {
        self.password.pop();
        self.message = "Enter password".to_string();
    }

    pub fn clear_password(&mut self) {
        self.password.clear();
        self.password_visible = false;
    }

    pub fn toggle_password_visibility(&mut self) {
        self.password_visible = !self.password_visible;
    }

    pub fn pulse(&mut self, kind: LockPulseKind) {
        self.pulse = Some(LockPulse {
            kind,
            started_at: Instant::now(),
        });
    }

    pub fn pulse_frame(&self, now: Instant) -> Option<LockPulseFrame> {
        let pulse = self.pulse?;
        let elapsed = now.saturating_duration_since(pulse.started_at);
        (elapsed < LOCK_PULSE_DURATION).then_some(LockPulseFrame {
            kind: pulse.kind,
            elapsed,
        })
    }

    pub fn snapshot(&self, now: Instant) -> LockScreenSnapshot {
        LockScreenSnapshot {
            active: self.active,
            password_len: self.password.chars().count(),
            password_visible: self.password_visible,
            password_text: if self.password_visible {
                self.password.clone()
            } else {
                String::new()
            },
            message: self.message.clone(),
            authenticating: self.authenticating,
            pulse: self.pulse_frame(now),
        }
    }
}

impl Default for LockScreenState {
    fn default() -> Self {
        Self::new()
    }
}

#[repr(C)]
struct PamHandle {
    _private: [u8; 0],
}

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

#[repr(C)]
struct PamConv {
    conv: Option<
        unsafe extern "C" fn(
            c_int,
            *mut *const PamMessage,
            *mut *mut PamResponse,
            *mut c_void,
        ) -> c_int,
    >,
    appdata_ptr: *mut c_void,
}

type PamStart = unsafe extern "C" fn(
    *const c_char,
    *const c_char,
    *const PamConv,
    *mut *mut PamHandle,
) -> c_int;
type PamAuthenticate = unsafe extern "C" fn(*mut PamHandle, c_int) -> c_int;
type PamAcctMgmt = unsafe extern "C" fn(*mut PamHandle, c_int) -> c_int;
type PamEnd = unsafe extern "C" fn(*mut PamHandle, c_int) -> c_int;
type PamStrError = unsafe extern "C" fn(*mut PamHandle, c_int) -> *const c_char;

struct PamApi {
    lib: *mut c_void,
    start: PamStart,
    authenticate: PamAuthenticate,
    acct_mgmt: PamAcctMgmt,
    end: PamEnd,
    strerror: PamStrError,
}

impl PamApi {
    fn load() -> Result<Self, String> {
        let path = CString::new("libpam.so.0").unwrap();
        let lib = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW) };
        if lib.is_null() {
            return Err(dl_error());
        }

        let api = unsafe {
            Self {
                lib,
                start: load_symbol(lib, "pam_start")?,
                authenticate: load_symbol(lib, "pam_authenticate")?,
                acct_mgmt: load_symbol(lib, "pam_acct_mgmt")?,
                end: load_symbol(lib, "pam_end")?,
                strerror: load_symbol(lib, "pam_strerror")?,
            }
        };

        Ok(api)
    }
}

impl Drop for PamApi {
    fn drop(&mut self) {
        if !self.lib.is_null() {
            unsafe {
                libc::dlclose(self.lib);
            }
        }
    }
}

unsafe fn load_symbol<T: Copy>(lib: *mut c_void, name: &str) -> Result<T, String> {
    let name = CString::new(name).unwrap();
    let symbol = libc::dlsym(lib, name.as_ptr());
    if symbol.is_null() {
        Err(dl_error())
    } else {
        Ok(std::mem::transmute_copy(&symbol))
    }
}

fn dl_error() -> String {
    let err = unsafe { libc::dlerror() };
    if err.is_null() {
        "dynamic linker error".to_string()
    } else {
        unsafe { CStr::from_ptr(err) }
            .to_string_lossy()
            .into_owned()
    }
}

unsafe extern "C" fn pam_conversation(
    num_msg: c_int,
    msg: *mut *const PamMessage,
    resp: *mut *mut PamResponse,
    appdata_ptr: *mut c_void,
) -> c_int {
    if num_msg <= 0 || msg.is_null() || resp.is_null() || appdata_ptr.is_null() {
        return PAM_CONV_ERR;
    }

    let password = appdata_ptr.cast::<CString>().as_ref().unwrap();
    let responses =
        libc::calloc(num_msg as usize, std::mem::size_of::<PamResponse>()).cast::<PamResponse>();
    if responses.is_null() {
        return PAM_BUF_ERR;
    }

    for index in 0..num_msg as isize {
        let message = *msg.offset(index);
        if message.is_null() {
            continue;
        }

        let style = (*message).msg_style;
        if style == PAM_PROMPT_ECHO_OFF || style == PAM_PROMPT_ECHO_ON {
            let copy = libc::strdup(password.as_ptr());
            if copy.is_null() {
                libc::free(responses.cast());
                return PAM_BUF_ERR;
            }
            (*responses.offset(index)).resp = copy;
        }
    }

    *resp = responses;
    PAM_SUCCESS
}

pub fn authenticate_current_user(password: &str) -> Result<bool, String> {
    let user = current_username().ok_or_else(|| "could not determine current user".to_string())?;
    authenticate_user(&user, password)
}

fn authenticate_user(user: &str, password: &str) -> Result<bool, String> {
    let pam = PamApi::load()?;
    let service = CString::new("login").map_err(|err| err.to_string())?;
    let user = CString::new(user).map_err(|err| err.to_string())?;
    let password = CString::new(password).map_err(|_| "password contains NUL byte".to_string())?;
    let conv = PamConv {
        conv: Some(pam_conversation),
        appdata_ptr: (&password as *const CString).cast_mut().cast(),
    };
    let mut handle: *mut PamHandle = ptr::null_mut();

    let mut status = unsafe { (pam.start)(service.as_ptr(), user.as_ptr(), &conv, &mut handle) };
    if status == PAM_SUCCESS {
        status = unsafe { (pam.authenticate)(handle, 0) };
    }
    if status == PAM_SUCCESS {
        status = unsafe { (pam.acct_mgmt)(handle, 0) };
    }

    let error = if status == PAM_SUCCESS {
        None
    } else {
        Some(pam_error(&pam, handle, status))
    };

    if !handle.is_null() {
        unsafe {
            (pam.end)(handle, status);
        }
    }

    match error {
        None => Ok(true),
        Some(err) if err.is_empty() => Ok(false),
        Some(_) => Ok(false),
    }
}

fn pam_error(pam: &PamApi, handle: *mut PamHandle, status: c_int) -> String {
    if handle.is_null() {
        return String::new();
    }

    let ptr = unsafe { (pam.strerror)(handle, status) };
    if ptr.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    }
}

fn current_username() -> Option<String> {
    if let Ok(user) = std::env::var("USER") {
        if !user.is_empty() {
            return Some(user);
        }
    }

    let uid = unsafe { libc::geteuid() };
    let passwd = unsafe { libc::getpwuid(uid) };
    if passwd.is_null() {
        return None;
    }

    let name = unsafe { (*passwd).pw_name };
    if name.is_null() {
        None
    } else {
        Some(
            unsafe { CStr::from_ptr(name) }
                .to_string_lossy()
                .into_owned(),
        )
    }
}
