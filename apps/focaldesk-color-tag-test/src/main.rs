//! Minimal client for the staging `focaldesk_color_v1` protocol.
//!
//! Usage:
//! ```text
//! WAYLAND_DISPLAY=focaldesk-0 focaldesk-color-tag-test --transfer linear
//! journalctl --user -b -g 'color tag' --no-pager
//! ```

mod protocol;

use anyhow::{bail, Context, Result};
use protocol::client::{focaldesk_color_manager_v1, focaldesk_surface_color_v1};
use std::env;
use std::ffi::CString;
use std::fs::File;
use std::os::fd::BorrowedFd;
use std::os::fd::FromRawFd;
use std::os::unix::io::AsRawFd;
use std::ptr;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_registry, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

const WIDTH: i32 = 256;
const HEIGHT: i32 = 256;
const STRIDE: i32 = WIDTH * 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransferTag {
    Srgb,
    Linear,
}

impl TransferTag {
    fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "srgb" | "s" => Ok(Self::Srgb),
            "linear" | "linear_srgb" | "l" => Ok(Self::Linear),
            other => bail!("unknown transfer `{other}` (expected srgb or linear)"),
        }
    }

    fn wire(self) -> focaldesk_surface_color_v1::Transfer {
        match self {
            Self::Srgb => focaldesk_surface_color_v1::Transfer::Srgb,
            Self::Linear => focaldesk_surface_color_v1::Transfer::LinearSrgb,
        }
    }
}

struct App {
    transfer: TransferTag,
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    wm_base: Option<xdg_wm_base::XdgWmBase>,
    color_manager: Option<focaldesk_color_manager_v1::FocaldeskColorManagerV1>,
    surface: Option<wl_surface::WlSurface>,
    configured: bool,
    done: bool,
}

impl App {
    fn new(transfer: TransferTag) -> Self {
        Self {
            transfer,
            compositor: None,
            shm: None,
            wm_base: None,
            color_manager: None,
            surface: None,
            configured: false,
            done: false,
        }
    }

    fn create_window(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        let compositor = self.compositor.as_ref().context("wl_compositor missing")?;
        let wm_base = self.wm_base.as_ref().context("xdg_wm_base missing")?;
        let color_manager = self
            .color_manager
            .as_ref()
            .context("focaldesk_color_manager_v1 missing (is this FocalDesk?)")?;

        let surface = compositor.create_surface(qh, ());
        let color_tag = color_manager.get_surface(&surface, qh, ());
        color_tag.set_transfer(self.transfer.wire());

        let xdg_surface = wm_base.get_xdg_surface(&surface, qh, ());
        let toplevel = xdg_surface.get_toplevel(qh, ());
        toplevel.set_title(format!("focaldesk color tag test ({:?})", self.transfer));

        self.surface = Some(surface);
        Ok(())
    }

    fn attach_buffer(&self, surface: &wl_surface::WlSurface, qh: &QueueHandle<Self>) -> Result<()> {
        let shm = self.shm.as_ref().context("wl_shm missing")?;
        let size = (STRIDE * HEIGHT) as usize;
        let memfd = create_memfd(size)?;
        let fd = memfd.as_raw_fd();
        let mut mapping = ShmMapping::map(fd, size, memfd)?;
        fill_test_pattern(mapping.as_mut(), self.transfer);

        let pool = shm.create_pool(unsafe { BorrowedFd::borrow_raw(fd) }, size as i32, qh, ());
        let buffer = pool.create_buffer(0, WIDTH, HEIGHT, STRIDE, wl_shm::Format::Argb8888, qh, ());
        surface.attach(Some(&buffer), 0, 0);
        std::mem::forget((pool, buffer, mapping));
        Ok(())
    }

    fn commit_if_ready(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        if !self.configured {
            return Ok(());
        }
        let Some(surface) = self.surface.as_ref() else {
            return Ok(());
        };
        self.attach_buffer(surface, qh)?;
        surface.commit();
        Ok(())
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for App {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for App {
    fn event(
        _: &mut Self,
        _: &wl_compositor::WlCompositor,
        _: wl_compositor::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm::WlShm, ()> for App {
    fn event(
        _: &mut Self,
        _: &wl_shm::WlShm,
        _: wl_shm::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for App {
    fn event(
        _: &mut Self,
        _: &wl_shm_pool::WlShmPool,
        _: wl_shm_pool::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for App {
    fn event(
        _: &mut Self,
        _: &wl_buffer::WlBuffer,
        _: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for App {
    fn event(
        _: &mut Self,
        _: &wl_surface::WlSurface,
        _: wl_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for App {
    fn event(
        _: &mut Self,
        wm_base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for App {
    fn event(
        app: &mut Self,
        xdg_surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            app.configured = true;
            let _ = app.commit_if_ready(qh);
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for App {
    fn event(
        app: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, xdg_toplevel::Event::Close) {
            app.done = true;
        }
    }
}

impl Dispatch<focaldesk_color_manager_v1::FocaldeskColorManagerV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &focaldesk_color_manager_v1::FocaldeskColorManagerV1,
        _: focaldesk_color_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<focaldesk_surface_color_v1::FocaldeskSurfaceColorV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &focaldesk_surface_color_v1::FocaldeskSurfaceColorV1,
        _: focaldesk_surface_color_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

struct ShmMapping {
    ptr: *mut u8,
    len: usize,
    _memfd: File,
}

impl ShmMapping {
    fn map(fd: i32, len: usize, memfd: File) -> Result<Self> {
        let ptr = unsafe {
            libc::mmap(
                ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            bail!("mmap failed");
        }
        Ok(Self {
            ptr: ptr as *mut u8,
            len,
            _memfd: memfd,
        })
    }

    fn as_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for ShmMapping {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr as *mut _, self.len);
        }
    }
}

fn create_memfd(size: usize) -> Result<File> {
    let name = CString::new("focaldesk-color-tag-test")?;
    let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        bail!("memfd_create failed");
    }
    let file = unsafe { File::from_raw_fd(fd) };
    file.set_len(size as u64).context("failed to size memfd")?;
    Ok(file)
}

fn fill_test_pattern(pixels: &mut [u8], transfer: TransferTag) {
    for y in 0..HEIGHT as usize {
        for x in 0..WIDTH as usize {
            let idx = (y * STRIDE as usize) + (x * 4);
            let (r, g, b) = if x < WIDTH as usize / 2 {
                encoded_green(transfer, 0.18)
            } else {
                encoded_green(transfer, 0.50)
            };
            pixels[idx] = b;
            pixels[idx + 1] = g;
            pixels[idx + 2] = r;
            pixels[idx + 3] = 255;
        }
    }
}

fn encoded_green(transfer: TransferTag, level: f32) -> (u8, u8, u8) {
    let byte = match transfer {
        TransferTag::Linear => (level.clamp(0.0, 1.0) * 255.0).round() as u8,
        TransferTag::Srgb => (linear_to_srgb(level).clamp(0.0, 1.0) * 255.0).round() as u8,
    };
    (0, byte, 0)
}

fn linear_to_srgb(value: f32) -> f32 {
    if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.max(0.0).powf(1.0 / 2.4) - 0.055
    }
}

fn parse_args() -> Result<TransferTag> {
    let mut transfer = TransferTag::Linear;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--transfer" | "-t" => {
                transfer = TransferTag::parse(
                    &args
                        .next()
                        .context("--transfer requires a value (srgb or linear)")?,
                )?;
            }
            other => bail!("unknown argument `{other}`"),
        }
    }
    Ok(transfer)
}

fn print_help() {
    println!("focaldesk-color-tag-test — exercise focaldesk_color_v1");
    println!();
    println!("Usage:");
    println!("  focaldesk-color-tag-test [--transfer linear|srgb]");
    println!();
    println!("Environment:");
    println!("  WAYLAND_DISPLAY   FocalDesk compositor socket (required)");
}

fn main() -> Result<()> {
    let transfer = parse_args()?;
    let conn = Connection::connect_to_env().context("failed to connect to Wayland display")?;
    let (globals, mut event_queue) =
        registry_queue_init::<App>(&conn).context("failed to read Wayland registry")?;
    let qh = event_queue.handle();
    let registry = globals.registry();

    let mut app = App::new(transfer);
    for global in globals.contents().clone_list() {
        match global.interface.as_str() {
            "wl_compositor" => {
                let compositor = registry.bind(global.name, global.version.min(4), &qh, ());
                app.compositor = Some(compositor);
            }
            "wl_shm" => {
                let shm = registry.bind(global.name, global.version.min(1), &qh, ());
                app.shm = Some(shm);
            }
            "xdg_wm_base" => {
                let wm_base = registry.bind(global.name, global.version.min(4), &qh, ());
                app.wm_base = Some(wm_base);
            }
            "focaldesk_color_manager_v1" => {
                let manager = registry.bind(global.name, global.version.min(1), &qh, ());
                app.color_manager = Some(manager);
            }
            _ => {}
        }
    }

    event_queue
        .roundtrip(&mut app)
        .context("initial roundtrip failed")?;

    app.create_window(&qh)?;
    event_queue
        .roundtrip(&mut app)
        .context("window setup roundtrip failed")?;

    eprintln!(
        "focaldesk-color-tag-test running with transfer={:?}; close the window to exit",
        app.transfer
    );

    while !app.done {
        event_queue
            .blocking_dispatch(&mut app)
            .context("event dispatch failed")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_and_srgb_encodings_differ_for_same_level() {
        let linear = encoded_green(TransferTag::Linear, 0.18);
        let srgb = encoded_green(TransferTag::Srgb, 0.18);
        assert_ne!(linear, srgb);
    }
}
