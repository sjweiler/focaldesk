//! Minimal client for `wp_color_management_v1`.
//!
//! ```text
//! WAYLAND_DISPLAY=focaldesk-0 focaldesk-wp-color-test --transfer linear
//! WAYLAND_DISPLAY=focaldesk-0 focaldesk-wp-color-test --chrome-path
//! ```

use anyhow::{bail, Context, Result};
use std::env;
use std::ffi::CString;
use std::fs::File;
use std::os::fd::{BorrowedFd, FromRawFd};
use std::os::unix::io::AsRawFd;
use std::ptr;
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_output, wl_registry, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::wp::color_management::v1::client::{
    wp_color_management_output_v1, wp_color_management_surface_feedback_v1,
    wp_color_management_surface_v1, wp_color_manager_v1, wp_image_description_creator_params_v1,
    wp_image_description_info_v1, wp_image_description_v1,
};
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TestMode {
    Simple,
    ChromePath,
}

struct App {
    mode: TestMode,
    transfer: TransferTag,
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    wm_base: Option<xdg_wm_base::XdgWmBase>,
    output: Option<wl_output::WlOutput>,
    color_manager: Option<wp_color_manager_v1::WpColorManagerV1>,
    color_output: Option<wp_color_management_output_v1::WpColorManagementOutputV1>,
    output_image: Option<wp_image_description_v1::WpImageDescriptionV1>,
    output_info_done: bool,
    color_feedback:
        Option<wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1>,
    preferred_image: Option<wp_image_description_v1::WpImageDescriptionV1>,
    preferred_info_done: bool,
    image_description: Option<wp_image_description_v1::WpImageDescriptionV1>,
    image_ready: bool,
    manager_done: bool,
    chrome_output_ready: bool,
    surface: Option<wl_surface::WlSurface>,
    configured: bool,
    done: bool,
}

impl App {
    fn new(mode: TestMode, transfer: TransferTag) -> Self {
        Self {
            mode,
            transfer,
            compositor: None,
            shm: None,
            wm_base: None,
            output: None,
            color_manager: None,
            color_output: None,
            output_image: None,
            output_info_done: false,
            color_feedback: None,
            preferred_image: None,
            preferred_info_done: false,
            image_description: None,
            image_ready: false,
            manager_done: false,
            chrome_output_ready: false,
            surface: None,
            configured: false,
            done: false,
        }
    }

    fn manager(&self) -> Result<&wp_color_manager_v1::WpColorManagerV1> {
        self.color_manager
            .as_ref()
            .context("wp_color_manager_v1 missing (is this FocalDesk?)")
    }

    fn chrome_output_path_ready(&self) -> bool {
        !matches!(self.mode, TestMode::ChromePath) || self.chrome_output_ready
    }

    fn chrome_preferred_path_ready(&self) -> bool {
        !matches!(self.mode, TestMode::ChromePath) || self.preferred_info_done
    }

    fn setup_chrome_output_path(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        let manager = self.manager()?;
        let output = self.output.as_ref().context("wl_output missing")?;
        let color_output = manager.get_output(output, qh, ());
        let output_image = color_output.get_image_description(qh, ());
        self.color_output = Some(color_output);
        self.output_image = Some(output_image);
        Ok(())
    }

    fn create_image_description(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        let manager = self.manager()?;
        let creator = manager.create_parametric_creator(qh, ());
        creator.set_primaries_named(wp_color_manager_v1::Primaries::Srgb);
        match self.transfer {
            TransferTag::Srgb => {
                creator.set_tf_named(wp_color_manager_v1::TransferFunction::Bt1886);
            }
            TransferTag::Linear => {
                creator.set_tf_named(wp_color_manager_v1::TransferFunction::ExtLinear);
            }
        }
        let image = creator.create(qh, ());
        self.image_description = Some(image);
        Ok(())
    }

    fn create_window(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        let compositor = self.compositor.as_ref().context("wl_compositor missing")?;
        let wm_base = self.wm_base.as_ref().context("xdg_wm_base missing")?;
        let manager = self.manager()?;
        let image = self
            .image_description
            .as_ref()
            .context("image description missing")?;

        let surface = compositor.create_surface(qh, ());
        let color_surface = manager.get_surface(&surface, qh, ());
        if matches!(self.mode, TestMode::ChromePath) {
            let feedback = manager.get_surface_feedback(&surface, qh, ());
            let preferred = feedback.get_preferred(qh, ());
            self.preferred_image = Some(preferred);
            self.color_feedback = Some(feedback);
        }
        color_surface.set_image_description(image, wp_color_manager_v1::RenderIntent::Perceptual);

        let xdg_surface = wm_base.get_xdg_surface(&surface, qh, ());
        let toplevel = xdg_surface.get_toplevel(qh, ());
        toplevel.set_title(format!(
            "wp color test ({:?}, {:?})",
            self.mode, self.transfer
        ));

        self.surface = Some(surface);
        Ok(())
    }

    fn commit_if_ready(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        if !self.configured || !self.image_ready {
            return Ok(());
        }
        if !self.chrome_output_path_ready() || !self.chrome_preferred_path_ready() {
            return Ok(());
        }
        let surface = self.surface.as_ref().context("surface missing")?;
        self.attach_buffer(surface, qh)?;
        surface.commit();
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
        Ok(())
    }
}

fn image_description_ready(event: wp_image_description_v1::Event) -> bool {
    matches!(
        event,
        wp_image_description_v1::Event::Ready { .. }
            | wp_image_description_v1::Event::Ready2 { .. }
    )
}

impl Dispatch<wl_registry::WlRegistry, wayland_client::globals::GlobalListContents> for App {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &wayland_client::globals::GlobalListContents,
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

impl Dispatch<wl_output::WlOutput, ()> for App {
    fn event(
        _: &mut Self,
        _: &wl_output::WlOutput,
        _: wl_output::Event,
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

impl Dispatch<wp_color_manager_v1::WpColorManagerV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &wp_color_manager_v1::WpColorManagerV1,
        event: wp_color_manager_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if matches!(event, wp_color_manager_v1::Event::Done) {
            app.manager_done = true;
            if matches!(app.mode, TestMode::ChromePath) {
                let _ = app.setup_chrome_output_path(qh);
            }
        }
    }
}

impl Dispatch<wp_color_management_output_v1::WpColorManagementOutputV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &wp_color_management_output_v1::WpColorManagementOutputV1,
        _: wp_color_management_output_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1, ()>
    for App
{
    fn event(
        app: &mut Self,
        feedback: &wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1,
        event: wp_color_management_surface_feedback_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let fetch_preferred = matches!(
            event,
            wp_color_management_surface_feedback_v1::Event::PreferredChanged { .. }
                | wp_color_management_surface_feedback_v1::Event::PreferredChanged2 { .. }
        );
        if fetch_preferred {
            let preferred = feedback.get_preferred(qh, ());
            app.preferred_image = Some(preferred);
            app.preferred_info_done = false;
        }
    }
}

impl Dispatch<wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1, ()>
    for App
{
    fn event(
        _: &mut Self,
        _: &wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1,
        _: wp_image_description_creator_params_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wp_image_description_info_v1::WpImageDescriptionInfoV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &wp_image_description_info_v1::WpImageDescriptionInfoV1,
        event: wp_image_description_info_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if !matches!(event, wp_image_description_info_v1::Event::Done) {
            return;
        }

        if app.output_image.is_some() && !app.output_info_done {
            app.output_info_done = true;
            app.chrome_output_ready = true;
            let _ = app.commit_if_ready(qh);
            return;
        }

        if app.preferred_image.is_some() && !app.preferred_info_done {
            app.preferred_info_done = true;
            let _ = app.commit_if_ready(qh);
        }
    }
}

impl Dispatch<wp_image_description_v1::WpImageDescriptionV1, ()> for App {
    fn event(
        app: &mut Self,
        image: &wp_image_description_v1::WpImageDescriptionV1,
        event: wp_image_description_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if !image_description_ready(event) {
            return;
        }

        if app
            .output_image
            .as_ref()
            .is_some_and(|output| output == image)
        {
            image.get_information(qh, ());
            return;
        }

        if app
            .preferred_image
            .as_ref()
            .is_some_and(|preferred| preferred == image)
        {
            image.get_information(qh, ());
            return;
        }

        app.image_ready = true;
        let _ = app.commit_if_ready(qh);
    }
}

impl Dispatch<wp_color_management_surface_v1::WpColorManagementSurfaceV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &wp_color_management_surface_v1::WpColorManagementSurfaceV1,
        _: wp_color_management_surface_v1::Event,
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
    let name = CString::new("focaldesk-wp-color-test")?;
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

struct ParsedArgs {
    mode: TestMode,
    transfer: TransferTag,
}

fn parse_args() -> Result<ParsedArgs> {
    let mut mode = TestMode::Simple;
    let mut transfer = TransferTag::Linear;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--chrome-path" => {
                mode = TestMode::ChromePath;
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
    Ok(ParsedArgs { mode, transfer })
}

fn print_help() {
    println!("focaldesk-wp-color-test — exercise wp_color_management_v1");
    println!();
    println!("Usage:");
    println!("  focaldesk-wp-color-test [--transfer linear|srgb]");
    println!("  focaldesk-wp-color-test --chrome-path [--transfer linear|srgb]");
}

fn main() -> Result<()> {
    let ParsedArgs { mode, transfer } = parse_args()?;
    let conn = Connection::connect_to_env().context("failed to connect to Wayland display")?;
    let (globals, mut event_queue) =
        registry_queue_init::<App>(&conn).context("failed to read Wayland registry")?;
    let qh = event_queue.handle();
    let registry = globals.registry();

    let manager_version = match mode {
        TestMode::Simple => 2,
        TestMode::ChromePath => 1,
    };

    let mut app = App::new(mode, transfer);
    for global in globals.contents().clone_list() {
        match global.interface.as_str() {
            "wl_compositor" => {
                app.compositor = Some(registry.bind(global.name, global.version.min(4), &qh, ()));
            }
            "wl_shm" => {
                app.shm = Some(registry.bind(global.name, global.version.min(1), &qh, ()));
            }
            "wl_output" if app.output.is_none() => {
                app.output = Some(registry.bind(global.name, global.version.min(4), &qh, ()));
            }
            "xdg_wm_base" => {
                app.wm_base = Some(registry.bind(global.name, global.version.min(4), &qh, ()));
            }
            "wp_color_manager_v1" => {
                app.color_manager =
                    Some(registry.bind(global.name, global.version.min(manager_version), &qh, ()));
            }
            _ => {}
        }
    }

    event_queue
        .roundtrip(&mut app)
        .context("initial roundtrip failed")?;
    if !app.manager_done {
        bail!("wp_color_manager_v1 did not send done");
    }

    if matches!(mode, TestMode::ChromePath) && app.output.is_none() {
        bail!("--chrome-path requires wl_output");
    }

    if matches!(mode, TestMode::ChromePath) {
        event_queue
            .roundtrip(&mut app)
            .context("output image description roundtrip failed")?;
        if !app.chrome_output_ready {
            bail!("output wp_image_description_v1 path did not complete");
        }
    }

    app.create_image_description(&qh)?;
    event_queue
        .roundtrip(&mut app)
        .context("image description roundtrip failed")?;
    if !app.image_ready {
        bail!("wp_image_description_v1 did not become ready");
    }

    app.create_window(&qh)?;
    event_queue
        .roundtrip(&mut app)
        .context("window roundtrip failed")?;

    if matches!(mode, TestMode::ChromePath) && !app.preferred_info_done {
        bail!("surface feedback get_preferred path did not complete");
    }

    println!(
        "focaldesk-wp-color-test running mode={mode:?} transfer={transfer:?}; close the window to exit",
    );

    while !app.done {
        event_queue
            .blocking_dispatch(&mut app)
            .context("event dispatch failed")?;
    }

    Ok(())
}
