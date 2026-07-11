//! Console VT switching for the greeter/session seat.
//!
//! Same idea as greetd's `[terminal] switch = true`: the greeter must run on
//! the *active* seat VT or libseat/logind will open `/dev/dri/card*` without
//! DRM master, and NVIDIA scanout allocation / `set_crtc` fail.

use anyhow::{bail, Context as _};
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;

/// `linux/vt.h` — activate the given VT as the foreground console.
const VT_ACTIVATE: libc::c_ulong = 0x5606;
/// Block until the given VT is the foreground console.
const VT_WAITACTIVE: libc::c_ulong = 0x5607;

/// Switch the system console to `vt` and wait until it is active.
///
/// Must run as root. Uses `/dev/tty0` (the current virtual console control
/// device), not `/dev/ttyN`, so the ioctl targets the VT subsystem itself.
pub fn switch_to(vt: u32) -> anyhow::Result<()> {
    if vt == 0 || vt > 63 {
        bail!("invalid VT number {vt}");
    }

    let tty0 = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty0")
        .context("open /dev/tty0 for VT switch")?;
    let fd = tty0.as_raw_fd();

    // SAFETY: fd is an open /dev/tty0; VT_ACTIVATE/VT_WAITACTIVE take the VT
    // number as the ioctl argument.
    let activate = unsafe { libc::ioctl(fd, VT_ACTIVATE, vt as libc::c_ulong) };
    if activate != 0 {
        return Err(std::io::Error::last_os_error()).context(format!("VT_ACTIVATE({vt})"));
    }

    let wait = unsafe { libc::ioctl(fd, VT_WAITACTIVE, vt as libc::c_ulong) };
    if wait != 0 {
        return Err(std::io::Error::last_os_error()).context(format!("VT_WAITACTIVE({vt})"));
    }

    tracing::info!(vt, "switched console to greeter VT");
    Ok(())
}
