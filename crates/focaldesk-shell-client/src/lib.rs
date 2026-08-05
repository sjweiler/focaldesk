//! Shared runtime for FocalDesk trusted shell clients.
//!
//! This first migration slice presents real `wlr-layer-shell` surfaces with
//! exclusive zones while the legacy compositor chrome remains enabled. That
//! makes the client boundary testable without making a failed shell startup
//! blank the desktop.

use anyhow::{Context, Result};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm, wl_surface},
    Connection, QueueHandle,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellRole {
    Panel,
    Dock,
}

impl ShellRole {
    pub const fn namespace(self) -> &'static str {
        match self {
            Self::Panel => "focal-panel",
            Self::Dock => "focal-dock",
        }
    }
    const fn anchor(self) -> Anchor {
        match self {
            Self::Panel => Anchor::TOP.union(Anchor::LEFT).union(Anchor::RIGHT),
            Self::Dock => Anchor::TOP.union(Anchor::LEFT).union(Anchor::BOTTOM),
        }
    }
    const fn preferred_size(self) -> (u32, u32) {
        match self {
            Self::Panel => (0, 64),
            Self::Dock => (76, 0),
        }
    }
    const fn color(self) -> [u8; 4] {
        match self {
            // The compositor's legacy chrome remains visible during this
            // migration slice; the client owns reservation, not appearance yet.
            Self::Panel | Self::Dock => [0, 0, 0, 0],
        }
    }
}

/// Run a trusted FocalDesk shell client until the compositor closes it.
pub fn run(role: ShellRole) -> Result<()> {
    let conn = Connection::connect_to_env().context("connect to Wayland compositor")?;
    let (globals, mut event_queue) = registry_queue_init(&conn).context("read Wayland globals")?;
    let qh = event_queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).context("wl_compositor unavailable")?;
    let layer_shell = LayerShell::bind(&globals, &qh).context("wlr-layer-shell unavailable")?;
    let shm = Shm::bind(&globals, &qh).context("wl_shm unavailable")?;
    let pool = SlotPool::new(1024 * 1024, &shm).context("create shared-memory pool")?;
    let surface = compositor.create_surface(&qh);
    let layer =
        layer_shell.create_layer_surface(&qh, surface, Layer::Top, Some(role.namespace()), None);
    let (width, height) = role.preferred_size();
    layer.set_anchor(role.anchor());
    layer.set_exclusive_zone(if width == 0 {
        height as i32
    } else {
        width as i32
    });
    layer.set_size(width, height);
    layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
    layer.commit();

    let mut client = ShellClient {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        shm,
        pool,
        layer,
        role,
        width,
        height,
        configured: false,
        closed: false,
    };
    while !client.closed {
        event_queue
            .blocking_dispatch(&mut client)
            .context("dispatch shell client events")?;
    }
    Ok(())
}

struct ShellClient {
    registry_state: RegistryState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    role: ShellRole,
    width: u32,
    height: u32,
    configured: bool,
    closed: bool,
}

impl CompositorHandler for ShellClient {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.draw(qh);
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for ShellClient {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for ShellClient {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.closed = true;
    }
    fn configure(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        if configure.new_size.0 > 0 {
            self.width = configure.new_size.0;
        }
        if configure.new_size.1 > 0 {
            self.height = configure.new_size.1;
        }
        if !self.configured {
            self.configured = true;
            self.draw(qh);
        }
    }
}

impl ShmHandler for ShellClient {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ShellClient {
    fn draw(&mut self, qh: &QueueHandle<Self>) {
        let width = self.width.max(1);
        let height = self.height.max(1);
        let stride = width as i32 * 4;
        let (buffer, canvas) = self
            .pool
            .create_buffer(
                width as i32,
                height as i32,
                stride,
                wl_shm::Format::Argb8888,
            )
            .expect("create shell buffer");
        for pixel in canvas.chunks_exact_mut(4) {
            pixel.copy_from_slice(&self.role.color());
        }
        self.layer
            .wl_surface()
            .damage_buffer(0, 0, width as i32, height as i32);
        self.layer
            .wl_surface()
            .frame(qh, self.layer.wl_surface().clone());
        buffer
            .attach_to(self.layer.wl_surface())
            .expect("attach shell buffer");
        self.layer.commit();
    }
}

delegate_compositor!(ShellClient);
delegate_output!(ShellClient);
delegate_shm!(ShellClient);
delegate_layer!(ShellClient);
delegate_registry!(ShellClient);

impl ProvidesRegistryState for ShellClient {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}
