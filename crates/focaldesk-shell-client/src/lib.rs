//! Shared runtime for FocalDesk trusted shell clients.
//!
//! This first migration slice presents real `wlr-layer-shell` surfaces with
//! exclusive zones while the legacy compositor chrome remains enabled. That
//! makes the client boundary testable without making a failed shell startup
//! blank the desktop.

use anyhow::{Context, Result};
use focaldesk_ipc::{send_desktop_request, DesktopAction, IpcRequest, IpcResponse};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use std::time::{Duration, Instant};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
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
        seat_state: SeatState::new(&globals, &qh),
        shm,
        pool,
        layer,
        role,
        width,
        height,
        configured: false,
        closed: false,
        pointer: None,
        active_workspace: 1,
        shell: focaldesk_ipc::ShellSnapshot::default(),
        last_snapshot: Instant::now() - Duration::from_secs(10),
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
    seat_state: SeatState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    role: ShellRole,
    width: u32,
    height: u32,
    configured: bool,
    closed: bool,
    pointer: Option<wl_pointer::WlPointer>,
    active_workspace: u32,
    shell: focaldesk_ipc::ShellSnapshot,
    last_snapshot: Instant,
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

impl SeatHandler for ShellClient {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            self.pointer.take();
        }
    }
}

impl PointerHandler for ShellClient {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            if event.surface != *self.layer.wl_surface() {
                continue;
            }
            if let PointerEventKind::Press { .. } = event.kind {
                self.activate_at(event.position.0 as i32);
            }
        }
    }
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
    fn refresh_snapshot(&mut self) {
        if self.last_snapshot.elapsed() < Duration::from_millis(500) {
            return;
        }
        self.last_snapshot = Instant::now();
        let Ok(IpcResponse::DesktopSnapshot { snapshot }) =
            send_desktop_request(&IpcRequest::GetDesktopSnapshot)
        else {
            return;
        };
        self.active_workspace = snapshot.session.active_workspace_id.max(1);
        self.shell = snapshot.shell;
    }

    fn activate_at(&self, x: i32) {
        let action = match self.role {
            ShellRole::Panel if x >= self.width as i32 - 48 => DesktopAction::ToggleDoNotDisturb,
            ShellRole::Panel if x >= self.width as i32 - 96 => {
                DesktopAction::OpenNotificationsPanel
            }
            ShellRole::Panel if x >= self.width as i32 - 144 => DesktopAction::OpenSettingsPanel {
                panel: "power".into(),
            },
            ShellRole::Panel if x >= self.width as i32 - 192 => DesktopAction::OpenSettingsPanel {
                panel: "sound".into(),
            },
            ShellRole::Panel if x >= self.width as i32 - 240 => DesktopAction::OpenSettingsPanel {
                panel: "network".into(),
            },
            ShellRole::Panel => DesktopAction::FocusWorkspace { workspace: 1 },
            ShellRole::Dock => DesktopAction::OpenSettingsPanel {
                panel: "chrome".into(),
            },
        };
        let _ = send_desktop_request(&IpcRequest::ExecuteDesktopAction { action });
    }

    fn draw(&mut self, qh: &QueueHandle<Self>) {
        self.refresh_snapshot();
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
        if self.role == ShellRole::Panel {
            draw_text(
                canvas,
                width,
                16,
                18,
                &format!("WS {}", self.active_workspace),
            );
            let mut x = width.saturating_sub(120);
            if self.shell.do_not_disturb {
                draw_text(canvas, width, x, 18, "DND");
                x += 24;
            }
            if self.shell.network_carrier {
                draw_text(canvas, width, x, 18, "NET");
                x += 24;
            }
            draw_text(canvas, width, x, 18, "PWR");
            x += 24;
            if let Some(percent) = self.shell.battery_percent {
                draw_text(canvas, width, x, 18, &percent.to_string());
            }
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

fn draw_text(canvas: &mut [u8], width: u32, x: u32, y: u32, text: &str) {
    let mut cursor = x;
    for ch in text.chars() {
        let glyph = match ch {
            'W' => [
                0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001,
            ],
            'S' => [
                0b01110, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110,
            ],
            'D' => [
                0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110,
            ],
            'N' => [
                0b10001, 0b11001, 0b11001, 0b10101, 0b10011, 0b10011, 0b10001,
            ],
            'E' => [
                0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
            ],
            'T' => [
                0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
            ],
            'P' => [
                0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
            ],
            'R' => [
                0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
            ],
            '0'..='9' => digit_glyph(ch as u8 - b'0'),
            _ => [0; 7],
        };
        for (row, bits) in glyph.into_iter().enumerate() {
            for col in 0..5 {
                if bits & (1 << (4 - col)) == 0 {
                    continue;
                }
                let px = cursor + col;
                let py = y + row as u32;
                if px >= width {
                    continue;
                }
                let offset = ((py * width + px) * 4) as usize;
                if let Some(pixel) = canvas.get_mut(offset..offset + 4) {
                    pixel.copy_from_slice(&[235, 235, 235, 235]);
                }
            }
        }
        cursor += 6;
    }
}

const fn digit_glyph(digit: u8) -> [u8; 7] {
    match digit {
        0 => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        1 => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        2 => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        3 => [
            0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        4 => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        5 => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b00001, 0b00001, 0b11110,
        ],
        6 => [
            0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        7 => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        8 => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        _ => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b11100,
        ],
    }
}

delegate_compositor!(ShellClient);
delegate_output!(ShellClient);
delegate_shm!(ShellClient);
delegate_layer!(ShellClient);
delegate_pointer!(ShellClient);
delegate_seat!(ShellClient);
delegate_registry!(ShellClient);

impl ProvidesRegistryState for ShellClient {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}
