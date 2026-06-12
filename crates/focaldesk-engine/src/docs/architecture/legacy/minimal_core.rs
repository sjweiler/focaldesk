#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum KeyAction {
    CloseFocused,
    FocusNext,
    FocusPrev,
    OverflowView,
    QuitCompositor,
    ToggleLauncher,
    LaunchTerminal,
    ActivateSlot(usize),
    AssignSlot(usize),
}

use std::collections::HashSet;
use smithay::backend::renderer::element::AsRenderElements;
use crate::text::rasterize_text_to_texture;
use std::time::Instant;
use chrono::Local;
use focaldesk_logging::flog_info;

// Image decoding
use std::fs::File;
use std::io::Read;
use image::GenericImageView;

use crate::text::TextSystem;
use smithay::utils::Scale as Scale;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::ImportMem;
use smithay::backend::renderer::gles::{GlesFrame,GlesTexture};
use smithay::wayland::seat::WaylandFocus;
use smithay::desktop::{Space, Window};
use smithay::{delegate_output};
use smithay::wayland::output::{OutputHandler, OutputManagerState};
use smithay::output::{Output, Mode, PhysicalProperties, Subpixel};
use crate::layout::{LayoutConfig, LayoutEngine, LayoutSnapshot};
use smithay::utils::SERIAL_COUNTER;
use smithay::backend::input::ButtonState;
use std::process::Command;
use std::env;
use std::path::Path;
use bitflags::bitflags;
use std::rc::Rc;
use std::cell::Cell;
use flow::{FocalDesk, FlowEvent, FlowAction, WindowId};
use smithay::reexports::wayland_server::backend::ObjectId;
use std::collections::HashMap;
use smithay::backend::input::KeyState;
use smithay::input::keyboard::{KeysymHandle, ModifiersState, Keysym};
use smithay::input::keyboard::keysyms;


use wayland_server::Resource;
use std::sync::Arc;

use smithay::reexports::winit;
use smithay::backend::winit as winit_backend; // Smithay backend glue (has init, WinitEvent, etc.)
use smithay::backend::winit::{WinitEvent};    // (optional) import event type

use smithay::reexports::winit::platform::pump_events::{
    EventLoopExtPumpEvents,
    PumpStatus,
};

use smithay::{
    backend::{
        input::{InputEvent, KeyboardKeyEvent, PointerButtonEvent},
        renderer::{
            element::{
                surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
                Kind,
            },
            gles::GlesRenderer,
            utils::{draw_render_elements, on_commit_buffer_handler},
            Color32F, Frame, Renderer,
        },
    },
    delegate_compositor, delegate_data_device, delegate_seat, delegate_shm, delegate_xdg_shell,
    input::{keyboard::FilterResult, Seat, SeatHandler, SeatState},
    reexports::wayland_server::{protocol::wl_seat, Display},
    utils::{Rectangle, Size, Physical, Serial, Transform, Logical, Point},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            with_surface_tree_downward, CompositorClientState, CompositorHandler, CompositorState,
            SurfaceAttributes, TraversalAction,
        },
        selection::{
            data_device::{DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler},
            SelectionHandler,
        },
        shell::xdg::{PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState},
        shm::{ShmHandler, ShmState},
    },
};
use wayland_protocols::xdg::shell::server::xdg_toplevel;
use wayland_server::{
    backend::{ClientData, ClientId, DisconnectReason},
    protocol::{
        wl_buffer,
        wl_surface::{self, WlSurface},
    },
    Client, ListeningSocket,
};

use ab_glyph::FontArc;
use std::sync::OnceLock;

use crate::fonts::{
    IBMPLEX_REGULAR,
    IBMPLEX_MEDIUM,
    IBMPLEX_SEMIBOLD,
};

pub fn run_winit() -> Result<(), Box<dyn std::error::Error>> {
    // move/call your existing winit init + loop here
    Ok(())
}

static FONT_REGULAR: OnceLock<FontArc> = OnceLock::new();
static FONT_MEDIUM: OnceLock<FontArc> = OnceLock::new();
static FONT_SEMIBOLD: OnceLock<FontArc> = OnceLock::new();

pub fn font_regular() -> &'static FontArc {
    FONT_REGULAR.get_or_init(|| {
        FontArc::try_from_slice(IBMPLEX_REGULAR)
            .expect("failed to load IBMPlexSans-Regular.ttf")
    })
}

pub fn font_medium() -> &'static FontArc {
    FONT_MEDIUM.get_or_init(|| {
        FontArc::try_from_slice(IBMPLEX_MEDIUM)
            .expect("failed to load IBMPlexSans-Medium.ttf")
    })
}

pub fn font_semibold() -> &'static FontArc {
    FONT_SEMIBOLD.get_or_init(|| {
        FontArc::try_from_slice(IBMPLEX_SEMIBOLD)
            .expect("failed to load IBMPlexSans-Semibold.ttf")
    })
}

type FlowRenderElement = WaylandSurfaceRenderElement<GlesRenderer>;

type OutputId = u64; // or your chosen stable output key

fn rect_apply_flipped180(
    r: Rectangle<i32, Physical>,
    output_size: (i32, i32),
) -> Rectangle<i32, Physical> {
    let (W, H) = output_size;
    Rectangle::new(
        ((W - (r.loc.x + r.size.w)), (H - (r.loc.y + r.size.h))).into(),
        r.size,
    )
}

#[derive(Default)]
pub struct ClockCache {
    pub last_string: String,
    pub texture: Option<GlesTexture>,
    pub scale: f64,
}



pub struct ClockWidget {
    pub texture: Option<GlesTexture>,
    pub last_string: String,
    pub last_update: Instant,
}

impl ClockWidget {
    pub fn new() -> Self {
        Self {
            texture: None,
            last_string: String::new(),
            last_update: Instant::now(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Chrome {
    pub side_bar_w: i32,
    pub top_bar_h: i32,
}

impl Default for Chrome {
    fn default() -> Self {
        Self { side_bar_w: 72, top_bar_h: 36 }
    }
}

pub struct FrameCtx<'a> {
    pub output_size: (i32, i32), // physical pixels
    pub output_scale: Scale<f64>,       // fractional
    pub buffer_scale: i32,       // integer >= 1
    pub damage: &'a [Rectangle<i32, Physical>],
    pub work: Rectangle<i32, Logical>,
}





fn load_wallpaper(
    renderer: &mut GlesRenderer,
    path: &str,
) -> Option<GlesTexture> {
    let img = image::open(path).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = img.dimensions();

    renderer
        .import_memory(&rgba, Fourcc::Abgr8888, (w as i32, h as i32).into(), false)
        .ok()
}


fn rect_logical_to_physical(
    r: Rectangle<i32, Logical>,
    scale: f64,
) -> Rectangle<i32, Physical> {
    let s = scale.max(0.1);

    let x = ((r.loc.x as f64) * s).floor() as i32;
    let y = ((r.loc.y as f64) * s).floor() as i32;

    let w = ((r.size.w as f64) * s).ceil() as i32;
    let h = ((r.size.h as f64) * s).ceil() as i32;

    Rectangle::new((x, y).into(), (w, h).into())
}

pub fn bind_wayland_socket() -> (ListeningSocket, String) {
    // Optional override: allow user/testing to force a specific socket name.
    if let Ok(requested) = std::env::var("FLOW_WAYLAND_DISPLAY") {
        match ListeningSocket::bind(&requested) {
            Ok(sock) => {
                let name = sock
                    .socket_name()
                    .expect("socket_name missing after bind")
                    .to_string_lossy()
                    .into_owned();

                flog_info!("FlowOS listening on WAYLAND_DISPLAY={name} (forced)");
                return (sock, name);
            }
            Err(err) => {
                flog_info!(
                    "FLOW_WAYLAND_DISPLAY={requested} was set but bind failed ({err}); falling back to auto."
                );
            }
        }
    }

    // Default: auto-pick an available wayland-N.
    // bind_auto("wayland", 0..32) is fine; bump range if you want.
    let sock = ListeningSocket::bind_auto("wayland", 0..64)
        .expect("Failed to bind any wayland-N socket");

    let name = sock
        .socket_name()
        .expect("socket_name missing after bind_auto")
        .to_string_lossy()
        .into_owned();

    flog_info!("FlowOS listening on WAYLAND_DISPLAY={name}");
    (sock, name)
}


impl BufferHandler for App {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl XdgShellHandler for App {

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let key: ObjectId = surface.wl_surface().id();
        self.handle_toplevel_destroyed_by_key(key);
    }

fn new_toplevel(&mut self, surface: ToplevelSurface) {
    let wid = self.flow.alloc_window_id();

    // Create smithay Window wrapper
    let win = Window::new(surface.clone());

    // Track it in your maps
    self.tl_window.insert(wid, win.clone());

    let key = surface.wl_surface().id();
    self.wid_to_surface.insert(wid, key.clone());
    self.tl_surface.insert(key, surface.clone());
   

    self.flow.note_focus(wid);

    // --- IMPORTANT: initial configure ---
    // Pick an initial size; since you want max-to-work, use your layout snapshot.
    // (Adjust field names if yours differ.)
    let work_size: Size<i32, Logical> = self.layout.work_area.size;

    surface.with_pending_state(|state| {
        state.size = Some(work_size);
        state.states.set(xdg_toplevel::State::Activated);
        state.states.set(xdg_toplevel::State::Maximized);
    });

    surface.send_configure();
    // --- end configure ---

    // Guard ONLY the mapping-to-space call
    //let sid: ObjectId = surface.wl_surface().id();
    if let Some(cow) = win.wl_surface() {
        let sid = cow.as_ref().id();
        if self.mapped_surfaces.insert(sid) {
            let loc: Point<i32, Logical> = self.layout.work_area.loc;
            self.space.map_element(win.clone(), loc, true);
            self.space.raise_element(&win, true);
        } else {
            // tracing::debug!(sid, "already mapped surface, skipping map");
        }    
    }
    
   

    self.focus_window(wid);
    self.needs_redraw = true;
}




    

    
 /*   
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
    tracing::debug!("NEW TOPLEVEL CALLED");
        let key: ObjectId = surface.wl_surface().id();          // stable object identity
        let wid: WindowId = self.flow.allocate_window_id();     // your stable Flow id

    
        // store protocol handle (optional but useful for configure/maximize/etc)
        self.tl_surface.insert(key.clone(), surface.clone());
        self.wid_to_surface.insert(wid, key.clone());
        self.surface_to_wid.insert(key.clone(), wid);
        
        // create smithay window for rendering
        let win = Window::new(surface);
        
        let loc = self.layout.work_area.loc; // Logical coords
        self.space.map_element(win.clone(), loc, false);

        
        self.tl_window.insert(wid, win.clone());
        
       // Add to MRU stack
    //self.mru.push(wid);
    // self.flow.note_focus(wid);
     
    // Focus it (this moves it to top and sets keyboard focus)
    self.focus_window(wid);
        
       tracing::debug!("space elements={}", self.space.elements().count());
       
        // optional: bring to top & focus
        //self.space.raise_element(&win, true);
        // self.keyboard_focus = Some(wid); // if you track focus by wid
    
        //  Tell Flow
        let action = self.flow.handle(FlowEvent::WindowMapped { id: wid });
        self.apply_flow_action(action);
        
        


        //  Now configure to work area
        if let Some(tl) = self.tl_surface.get(&key) {
            self.configure_toplevel_to_work_area(tl);
        }
    } */


    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

   

    
    
    
    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {
        // Handle popup creation here
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        // Handle popup grab here
    }

    fn reposition_request(&mut self, _surface: PopupSurface, _positioner: PositionerState, _token: u32) {
        // Handle popup reposition here
    }
    
    
}



impl App {
    pub fn handle_resize(&mut self, size: Size<i32, Physical>,) {
      self.needs_redraw = true;
    }
    pub fn mark_redraw(&mut self) {
       self.needs_redraw = true;
    }
    pub fn handle_input_event(&mut self, input: InputEvent, keyboard: &smithay::input::keyboard::KeyboardHandle<App> ) 
    {
    }
    
    pub fn ensure_gpu_resources(&mut self,renderer: &mut GlesRenderer, ctx: &FrameCtx) {
    
    }

    pub fn set_output_from_nested(
        &mut self,
        size: Size<i32, Physical>,
        scale_factor: f64,
    ) {
        self.output_size = size;
        self.scale_factor = scale_factor;
        self.buffer_scale = scale_factor.round().max(1.0) as i32;

        self.update_layout_from_output();
        self.configure_all_to_work_area();
        self.needs_redraw = true;
    }
    pub fn new_drm(
        size: Size<i32, Physical>
    ) -> Self{
        let mut display = Display::<App>::new()?;
        let dh = display.handle();

         let output_size = size;
         let output_manager_state = OutputManagerState::new_with_xdg_output::<App>(dh);
        let compositor_state = CompositorState::new::<App>(dh);
        let shm_state = ShmState::new::<App>(dh, vec![]);
        let mut seat_state = SeatState::new();
        let seat = seat_state.new_wl_seat(dh, seat_name);

        let mut cfg = LayoutConfig::default();
        cfg.top_bar_h = 37;
        cfg.side_bar_w = 65;

        let layout_engine = LayoutEngine::new(cfg);
        let layout = layout_engine.compute(size.w, size.h);
         Self {
            chrome: Chrome::default(),
            compositor_state,
            xdg_shell_state: XdgShellState::new::<App>(dh),
            shm_state,
            seat_state,
            data_device_state: DataDeviceState::new::<App>(dh),
            seat,
            tl_surface: HashMap::new(),
            tl_window: HashMap::new(),
            space: Space::<Window>::default(),
            surface_to_wid: HashMap::new(),
            wid_to_surface: HashMap::new(),
            flow: FocalDesk::new(),
            output_size: (0, 0).into(),
            keybinds: HashMap::new(),
            launcher_open: false,
            wayland_display,
            scale_factor: 1.0,
            buffer_scale: 1,
            layout_engine,
            layout,
            output_manager_state,
            output: None,
            wallpaper_texture: None,
            scratch_damage: [Rectangle::from_loc_and_size((0, 0), (1, 1)); 8],
            scratch_damage_len: 0,
            active_output: 0,
            window_output: HashMap::new(),
            text: crate::text::TextSystem::new(),
            clock: ClockCache::default(),
            needs_redraw: true,
            mapped: HashSet::new(),
            mapped_surfaces: HashSet::new()
         }
    }
    pub fn new_wayland(
        dh: &smithay::reexports::wayland_server::DisplayHandle,
        wayland_display: String,
        seat_name: &str,
    ) -> Self {
    let mut display = Display::<App>::new()?;
    let dh = display.handle();

        let output_manager_state = OutputManagerState::new_with_xdg_output::<App>(dh);
        let compositor_state = CompositorState::new::<App>(dh);
        let shm_state = ShmState::new::<App>(dh, vec![]);
        let mut seat_state = SeatState::new();
        let seat = seat_state.new_wl_seat(dh, seat_name);

        let mut cfg = LayoutConfig::default();
        cfg.top_bar_h = 37;
        cfg.side_bar_w = 65;

        let layout_engine = LayoutEngine::new(cfg);
        let layout = layout_engine.compute(1, 1);

        Self {
            chrome: Chrome::default(),
            compositor_state,
            xdg_shell_state: XdgShellState::new::<App>(dh),
            shm_state,
            seat_state,
            data_device_state: DataDeviceState::new::<App>(dh),
            seat,
            tl_surface: HashMap::new(),
            tl_window: HashMap::new(),
            space: Space::<Window>::default(),
            surface_to_wid: HashMap::new(),
            wid_to_surface: HashMap::new(),
            flow: FocalDesk::new(),
            output_size: (0, 0).into(),
            keybinds: HashMap::new(),
            launcher_open: false,
            wayland_display,
            scale_factor: 1.0,
            buffer_scale: 1,
            layout_engine,
            layout,
            output_manager_state,
            output: None,
            wallpaper_texture: None,
            scratch_damage: [Rectangle::from_loc_and_size((0, 0), (1, 1)); 8],
            scratch_damage_len: 0,
            active_output: 0,
            window_output: HashMap::new(),
            text: crate::text::TextSystem::new(),
            clock: ClockCache::default(),
            needs_redraw: true,
            mapped: HashSet::new(),
            mapped_surfaces: HashSet::new(),
        }
    }
fn focus_fallback_after_close(&mut self) {
    // Focus most-recent valid remaining window
    for wid in self.flow.mru_vec().iter().rev().copied() {
        if self.tl_window.contains_key(&wid) {
            self.focus_window(wid);
            return;
        }
    }

    // None left: clear keyboard focus
    if let Some(kb) = self.seat.get_keyboard() {
        kb.set_focus(self, None, 0.into());
    }
}

fn map_window_once(&mut self, wid: WindowId, win: Window, loc: smithay::utils::Point<i32, smithay::utils::Logical>) {
    if !self.mapped.insert(wid) {
        tracing::debug!(?wid, "map_window_once: already mapped, skipping");
        return;
    }

    self.space.map_element(win.clone(), loc, true); // or map_window(...)
    self.space.raise_element(&win, true);
    self.needs_redraw = true;
}

    fn handle_toplevel_destroyed_by_key(&mut self, key: ObjectId) {
        // Resolve wid BEFORE removing mappings
        let wid = match self.surface_to_wid.get(&key).copied() {
            Some(w) => w,
            None => {
                tracing::warn!(?key, "toplevel_destroyed: unknown surface key");
                return;
            }
        };

        tracing::info!(?wid, ?key, "toplevel_destroyed");

        // Remove from Space while Window handle still exists
        if let Some(win) = self.tl_window.remove(&wid) {
            // NOTE: pick the correct call for YOUR smithay version:
            // self.space.unmap_elem(&win);
            // self.space.unmap_element(&win);
            // self.space.remove_element(&win);
            self.space.unmap_elem(&win);
        }

        // Remove protocol/object mappings
        self.surface_to_wid.remove(&key);
        self.wid_to_surface.remove(&wid);
        self.tl_surface.remove(&key);

        // Update Flow policy
        self.flow.remove_window(wid);
        
        // Fallback focus: pick most recent remaining that still exists
        if let Some(next) = self.flow.most_recent() {
            // only focus if compositor still has it
            if self.tl_window.contains_key(&next) {
                self.focus_window(next);
            } else {
                // if MRU can be stale, you’ll want an iterator-based fallback later
                tracing::warn!(?next, "fallback wid not in tl_window");
            }
        } else {
            // No windows left: clear keyboard focus
            if let Some(kb) = self.seat.get_keyboard() {
                kb.set_focus(self, None, 0.into());
            }
        }

        self.needs_redraw = true;
}

fn on_toplevel_destroyed(&mut self, key: ObjectId) {
    // 0) Resolve wid while mappings still exist
    let wid = match self.surface_to_wid.get(&key).copied() {
        Some(w) => w,
        None => {
            tracing::warn!(?key, "destroyed: unknown surface key");
            return;
        }
    };

    tracing::info!(?wid, ?key, "destroyed window");

    // 1) Remove from Space while you still have the Window handle
    if let Some(win) = self.tl_window.remove(&wid) {
        // use the right API for your smithay version:
        // self.space.unmap_elem(&win);
        // or self.space.unmap_element(&win);
        // or self.space.remove_element(&win);
        // (pick the one that exists)
        self.space.unmap_elem(&win);
    }

    // 2) Remove protocol maps
    self.surface_to_wid.remove(&key);
    self.wid_to_surface.remove(&wid);
    self.tl_surface.remove(&key);

    // 3) Update FocalDesk policy
    self.flow.remove_window(wid);

    // 4) Restore keyboard focus to a remaining window (fallback)
    self.focus_fallback_after_close();

    self.needs_redraw = true;
}

fn request_redraw_or_wakeup(&mut self) {
        self.needs_redraw = true;
}
    
fn clock_string(&mut self) -> String {
    let s = Local::now().format("%I:%M %p").to_string(); // "02:07 PM"
    s.trim_start_matches('0').to_string()                 // "2:07 PM"
}

fn ensure_clock_texture(&mut self, renderer: &mut GlesRenderer, scale: f64) {
    let now = self.clock_string(); // "14:37"

    if now != self.clock.last_string || self.clock.texture.is_none() || self.clock.scale != scale {
        self.clock.texture = rasterize_text_to_texture(renderer, &now, scale);
        self.clock.last_string = now;
        self.clock.scale = scale;
    }
}

fn resolve_focus_target(&self, wid: WindowId) -> Option<(Window, smithay::reexports::wayland_server::protocol::wl_surface::WlSurface)> {
    let win = self.tl_window.get(&wid)?.clone();

    let wl_surface = self
        .wid_to_surface
        .get(&wid)
        .and_then(|k| self.tl_surface.get(k))
        .map(|s| s.wl_surface().clone())?;

    Some((win, wl_surface))
}
fn focus_window(&mut self, wid: WindowId) {
    let Some(win) = self.tl_window.get(&wid).cloned() else {
        tracing::warn!(?wid, "focus_window: no tl_window");
        self.flow.remove_window(wid);
        if let Some(next) = self.flow.most_recent() {
            self.focus_window(next);
        }
        return;
    };

    self.flow.note_focus(wid);

    // visibility: bring to top
    self.space.raise_element(&win, true);

    // keyboard focus
    if let Some(keyboard) = self.seat.get_keyboard() {
        if let Some(wl_surface) = win.wl_surface() {
            keyboard.set_focus(self, Some(wl_surface.into_owned()), 0.into());
        } else {
            // Fallback (optional): focus nothing if surface missing
            keyboard.set_focus(self, None, 0.into());
        }
    }

    self.needs_redraw = true;
}
    



fn work_area_origin_logical(&self) -> Point<i32, Logical> {
        (self.chrome.side_bar_w, self.chrome.top_bar_h).into()
    }
// IMPLkeybinds
pub fn setup_keybinds(&mut self)
{
   self.keybinds.insert(
        KeyCombo { mods: ModMask::SUPER, sym: keysyms::KEY_Return },
        KeyAction::LaunchTerminal, // or whatever you call it
    );

    self.keybinds.insert(
        KeyCombo { mods: ModMask::SUPER, sym: keysyms::KEY_q },
        KeyAction::CloseFocused,
    );
    
    self.keybinds.insert(
        KeyCombo { mods: ModMask::SUPER, sym: keysyms::KEY_Q },
        KeyAction::CloseFocused,
    );

    self.keybinds.insert(
        KeyCombo { mods: ModMask::SUPER | ModMask::SHIFT, sym: keysyms::KEY_q },
        KeyAction::QuitCompositor,
    );
    
    self.keybinds.insert(
        KeyCombo { mods: ModMask::SUPER | ModMask::SHIFT, sym: keysyms::KEY_Q },
        KeyAction::QuitCompositor,
    );

    self.keybinds.insert(
        KeyCombo { mods: ModMask::SUPER, sym: keysyms::KEY_Tab },
        KeyAction::FocusNext,
    );

    self.keybinds.insert(
        KeyCombo { mods: ModMask::SUPER, sym: keysyms::KEY_space,},
        KeyAction::ToggleLauncher,
    );
    
    self.keybinds.insert(
        KeyCombo { mods: ModMask::ALT, sym: keysyms::KEY_0,},
        KeyAction::OverflowView,
    );
    
    self.keybinds.insert(
        KeyCombo { mods: ModMask::ALT, sym: keysyms::KEY_1,},
        KeyAction::ActivateSlot(0),
    );
    
    self.keybinds.insert(
        KeyCombo { mods: ModMask::ALT, sym: keysyms::KEY_2,},
        KeyAction::ActivateSlot(1),
    );
    
    self.keybinds.insert(
        KeyCombo { mods: ModMask::ALT, sym: keysyms::KEY_3,},
        KeyAction::ActivateSlot(2),
    );
    
    self.keybinds.insert(
        KeyCombo { mods: ModMask::ALT, sym: keysyms::KEY_4,},
        KeyAction::ActivateSlot(3),
    );
    
    self.keybinds.insert(
        KeyCombo { mods: ModMask::ALT, sym: keysyms::KEY_5,},
        KeyAction::ActivateSlot(4),
    );
    
    self.keybinds.insert(
        KeyCombo { mods: ModMask::ALT, sym: keysyms::KEY_6,},
        KeyAction::ActivateSlot(5),
    );
    
    self.keybinds.insert(
        KeyCombo { mods: ModMask::ALT, sym: keysyms::KEY_7,},
        KeyAction::ActivateSlot(6),
    );
    
    self.keybinds.insert(
        KeyCombo { mods: ModMask::ALT, sym: keysyms::KEY_8,},
        KeyAction::ActivateSlot(7),
    );
    
    self.keybinds.insert(
        KeyCombo { mods: ModMask::ALT, sym: keysyms::KEY_9,},
        KeyAction::ActivateSlot(8),
    );
    
    self.keybinds.insert(
        KeyCombo { mods: ModMask::ALT | ModMask::SHIFT, sym: keysyms::KEY_1,},
        KeyAction::AssignSlot(0),
    );
    
    self.keybinds.insert(
        KeyCombo { mods: ModMask::ALT | ModMask::SHIFT, sym: keysyms::KEY_2,},
        KeyAction::AssignSlot(1),
    );
    
    self.keybinds.insert(
        KeyCombo { mods: ModMask::ALT | ModMask::SHIFT, sym: keysyms::KEY_3,},
        KeyAction::AssignSlot(2),
    );
    
    self.keybinds.insert(
        KeyCombo { mods: ModMask::ALT | ModMask::SHIFT, sym: keysyms::KEY_4,},
        KeyAction::AssignSlot(3),
    );    
    
    self.keybinds.insert(
        KeyCombo { mods: ModMask::ALT | ModMask::SHIFT, sym: keysyms::KEY_5,},
        KeyAction::AssignSlot(4),
    );
    
    self.keybinds.insert(
        KeyCombo { mods: ModMask::ALT | ModMask::SHIFT, sym: keysyms::KEY_6,},
        KeyAction::AssignSlot(5),
    );
    
    self.keybinds.insert(
        KeyCombo { mods: ModMask::ALT | ModMask::SHIFT, sym: keysyms::KEY_7,},
        KeyAction::AssignSlot(6),
    );
    
    self.keybinds.insert(
        KeyCombo { mods: ModMask::ALT | ModMask::SHIFT, sym: keysyms::KEY_8,},
        KeyAction::AssignSlot(7),
    );
    
    self.keybinds.insert(
        KeyCombo { mods: ModMask::ALT | ModMask::SHIFT, sym: keysyms::KEY_9,},
        KeyAction::AssignSlot(8),
    );
    
    self.keybinds.insert(
        KeyCombo { mods: ModMask::ALT, sym: keysyms::KEY_Tab,},
        KeyAction::FocusNext,
    );
    
    self.keybinds.insert(
        KeyCombo { mods: ModMask::ALT | ModMask::SHIFT, sym: keysyms::KEY_Tab,},
        KeyAction::FocusPrev,
    );
}

// rendering stuff
pub fn render(
    &mut self,
    frame: &mut GlesFrame,
    ctx: &FrameCtx,
    layout: &LayoutSnapshot,
    elements: &[FlowRenderElement],
) {
    self.render_background(frame, ctx);         // layer 0
    self.render_clients(frame, ctx, elements);  // layer 1
    self.render_chrome(frame, ctx);             // layer 2  

}
pub fn render_into_frame(
    &mut self,
    frame: &mut GlesFrame,
    ctx: &FrameCtx,
    layout: &LayoutSnapshot,
    ) {
    self.render_background(frame, ctx);         // layer 0
    self.render_clients(frame, ctx, elements);  // layer 1
    self.render_chrome(frame, ctx);             // layer 2  
    
}

pub fn send_frame_callbacks(&self, time_ms: u32) {
    for surface in self.xdg_shell_state.toplevel_surfaces().iter() {
        let surface: &ToplevelSurface = surface;
        send_frames_surface_tree(surface.wl_surface(), time_ms);
    }
}


pub fn build_elements(&self, renderer: &mut GlesRenderer, ctx: &FrameCtx) -> Vec<FlowRenderElement> {
    let sf = ctx.output_scale.x;
    let mut out = Vec::new();

    // Draw from back -> front in Space stacking order
    tracing::info!(mru_len=self.flow.mru_vec().len(),
               space_count=self.space.elements().count(),
               "BUILD_ELEMENTS start");

    // Get windows in stacking order (top to bottom) or whatever you prefer.
    // If your Space API differs, adapt the iterator call to your smithay version.
    
    for (i, window) in self.space.elements().enumerate() {
        let loc_l: smithay::utils::Point<i32, Logical> = self
            .space
            .element_location(window)
            .unwrap_or_else(|| (0, 0).into());

        let loc: smithay::utils::Point<i32, Physical> =
            loc_l.to_physical_precise_round(ctx.output_scale);

        let scale = ctx.output_scale;

        // Keep as Option<ObjectId> (or Option<_>) for logging.
        let surf_id = window
            .wl_surface()
            .map(|cow| cow.as_ref().id());

        let elems = window.render_elements(renderer, loc, scale, 1.0);

        tracing::info!(i, ?surf_id, "DRAW ORDER");
        tracing::info!(n = elems.len(), "BUILD window elems");
        out.extend(elems);
    }

    out
}

/*
pub fn build_elements(
    &self,
    renderer: &mut GlesRenderer,
    ctx: &FrameCtx,
) -> Vec<FlowRenderElement> {
    let mut out: Vec<FlowRenderElement> = Vec::new();
    let sf = self.scale_factor; // f64
    // If you already map windows into “work-area coordinates”, you do NOT need ctx.work here.
    // If you *don’t*, and you want to shift everything into work-area, you can add work offset below.

     // Get windows in stacking order (top to bottom) or whatever you prefer.
    // If your Space API differs, adapt the iterator call to your smithay version.
    for window in self.space.elements() {
        // 1) Location: ask Space where this window is mapped
        let loc = self.space.element_location(window).unwrap_or((0, 0).into());

        // 2) Scale: use whatever you store for output scaling
        // If your ctx has a Scale<f64>, use it directly.
        let scale = ctx.output_scale;
        
        let elems = window.render_elements(renderer, loc, scale, 1.0);

        tracing::info!(n = elems.len(), "BUILD window elems");
        out.extend(elems);
    }

    out
}
*/



    pub fn build_frame_ctx<'a>(
        &self,
        output_size: (i32, i32),
        damage_buf: &'a mut [Rectangle<i32, Physical>],
    ) -> FrameCtx<'a> {
        // Compute full logical damage (example)
        let full_damage_l: Rectangle<i32, Logical> =
            Rectangle::from_loc_and_size((0, 0), (output_size.0, output_size.1));

        // Write into the caller-provided buffer
        damage_buf[0] = rect_logical_to_physical(full_damage_l, self.scale_factor);

        FrameCtx {
            output_size,
            output_scale: self.scale_factor.into(),
            buffer_scale: 1,                // or whatever you use
            work: self.layout.work_area,    // COPY the rect out of state
            damage: &damage_buf[..1],       // slice lives as long as damage_buf
        }
    }


fn render_clients(
    &mut self,
    frame: &mut GlesFrame,
    ctx: &FrameCtx,
    elements: &[FlowRenderElement],
) {
    //flog_info!("render_clients: elements.len={}", elements.len());
    //flog_info!(
 // "tl_window={} tl_surface={} space_elems={}",
 // self.tl_window.len(),
//  self.tl_surface.len(),
//  self.space.elements().count(), // if available in your Smithay version
//);

let full = smithay::utils::Rectangle::from_loc_and_size((0,0), ctx.output_size);
let damage = std::slice::from_ref(&full);

    draw_render_elements(
        frame,
        ctx.output_scale.x,   // ok if draw_render_elements wants f64
        elements,
        damage,
    )
    .unwrap();
}

 
 
 fn render_chrome(
    &mut self,
    frame: &mut GlesFrame,
    ctx: &FrameCtx,
) {
    let layout = &self.layout;

    let top_bar  = rect_logical_to_physical(layout.top_bar, self.scale_factor);
    let side_bar = rect_logical_to_physical(layout.side_bar, self.scale_factor);

    // If either rect becomes empty, skip it
    if top_bar.size.w > 0 && top_bar.size.h > 0 {
        frame.clear(Color32F::new(0.10, 0.12, 0.14, 1.0), &[top_bar]).unwrap();
    }
    if side_bar.size.w > 0 && side_bar.size.h > 0 {
        frame.clear(Color32F::new(0.12, 0.15, 0.18, 1.0), &[side_bar]).unwrap();
    }


    // Debug while you stabilize:
    // flog_info!("chrome: top={:?} side={:?} full={:?}", top_bar, side_bar, full);
}
   
fn ensure_wallpaper_loaded(&mut self, renderer: &mut GlesRenderer)
{
    if self.wallpaper_texture.is_none() {
        //flog_info!("wallpaper loaded");
        self.wallpaper_texture = load_wallpaper(
            renderer,
            "/home/steve/focusshell/assets/wallpaper/focaldesk_wallpaper.png",
            
    );
}
}



fn render_background(&mut self, frame: &mut GlesFrame, ctx: &FrameCtx) {
    use smithay::backend::renderer::Texture;
    use smithay::backend::renderer::gles::{GlesTexProgram, Uniform};
    use smithay::utils::{Rectangle, Transform};
    use smithay::utils::{Buffer, Logical, Physical};

    let full: Rectangle<i32, Physical> = Rectangle::new((0, 0).into(), ctx.output_size.into());
    let full_damage = [full];

    frame
        .clear(Color32F::new(0.07, 0.08, 0.10, 1.0), &full_damage)
        .unwrap();

    let Some(tex) = self.wallpaper_texture.as_ref() else {
        //flog_info!("no display wallpaper");
        return;
    };


    // ctx.work is Logical in your build
    let layout = &self.layout;
    
    let full: Rectangle<i32, Physical> = Rectangle::new((0, 0).into(), ctx.output_size.into());
    let full_damage = [full];

    // draw into FULL, not work
    let target: Rectangle<i32, Physical> = full;
    let ow = target.size.w;
    let oh = target.size.h;
    
    let sz = tex.size(); // Size<i32, Buffer>
    let tw = sz.w;
    let th = sz.h;

    if tw <= 0 || th <= 0 || ow <= 0 || oh <= 0 {
        return;
    }

    let sx = ow as f64 / tw as f64;
    let sy = oh as f64 / th as f64;
    let s = sx.max(sy); 

    let dw = (tw as f64 * s).ceil() as i32;
    let dh = (th as f64 * s).ceil() as i32;

    let x = full.loc.x + (ow - dw) / 2;
    let y = full.loc.y + (oh - dh) / 2;
    
    //let x = work.loc.x;
    //let y = work.loc.y;

    let dst_world = Rectangle::new((x, y).into(), (dw, dh).into());
    let dst = rect_apply_flipped180(dst_world, (ctx.output_size.0, ctx.output_size.1));
    let dsts = [dst];

    // src is f64 + Buffer per your earlier compiler message
let src: Rectangle<f64, Buffer> =
    Rectangle::new((0.0, 0.0).into(), (tw as f64, th as f64).into());

    frame
        .render_texture_from_to(
            tex,
            src,
            dst,            // <-- the missing single dst rect
            &dsts,          // <-- slice of dst rects
            &full_damage,   // damage
            Transform::Normal,
            1.0,
            None::<&GlesTexProgram>,
            &[] as &[Uniform<'_>],
        )
        .unwrap();
}
/*
let pos: Point<i32, Physical> = (x, y).into();
    // Draw the wallpaper (use full_damage so it always repaints cleanly)
    frame.render_texture_at(
        tex,
        pos,     // Point<i32, Physical>
        1,                 // buffer_scale: i32
        ctx.output_scale,  // output_scale: Into<Scale<f64>>  <-- THIS is what you're missing
        Transform::Normal, // transform
        &full_damage,      // damage slice  
        &[dst],               // destination rectangle     
        1.0,               // alpha
    ).unwrap();

}
*/

fn draw_texture_scaled_to_output(
    frame: &mut GlesFrame,
    texture: &GlesTexture,
    output_size: (i32, i32),
    output_scale: smithay::utils::Scale<f64>,
    damage: &[Rectangle<i32, Physical>],
) {
    let output_point: Point<i32, Physical> = (0, 0).into();
 let dst: Rectangle<i32, Physical> =
        Rectangle::from_loc_and_size((0, 0), (output_size.0, output_size.1));

    let buffer_scale: i32 = 1; // must be >= 1
    //let out_scale: smithay::utils::Scale<f64> = Scale::from(output_scale.max(1.0));
let full_damage = [dst];
    let damage = if damage.is_empty() { &full_damage[..] } else { damage };
    
    frame
        .render_texture_at(
            texture,
            output_point,
            buffer_scale,
            output_scale,
            Transform::Normal,
            damage,
            &[dst],
            1.0f32,
        )
        .unwrap();
}



    
    fn update_layout_from_output(&mut self) {
        // physical -> logical
        let phys = self.output_size;
        flog_info!("{:?}", self.output_size);
        
        let sf = self.scale_factor.max(0.1);

        flog_info!("scale factor {:?}",sf);

        let out_w = ((phys.w as f64) / sf).round() as i32;
        let out_h = ((phys.h as f64) / sf).round() as i32;

        self.layout = self.layout_engine.compute(out_w, out_h);
    }
    
    fn configure_all_toplevels_to_work_area(&self) {
        // This should be logical units (what clients expect)
        let sz = self.layout.client_size(); // Size<i32, Logical>
        let work_w = sz.w.max(1);
        let work_h = sz.h.max(1);

        //flog_info!("cfg max to work area (logical): {}x{}", work_w, work_h);

        for tl in self.tl_surface.values() {
            tl.with_pending_state(|st| {
                st.states.set(xdg_toplevel::State::Maximized);
                st.size = Some((work_w, work_h).into()); // (i32, i32) -> Size<i32, Logical>
            });
            tl.send_configure();
        }
    }
    
    fn set_focus(&mut self, wid: WindowId) {
        let sz = self.layout.client_size(); // Size<i32, Logical>
        let (work_w, work_h) = (sz.w, sz.h);

        
        

        // Update activated state for all toplevels, but ALWAYS keep them maximized to work area.
        for (id, key) in self.wid_to_surface.iter() {
            if let Some(tl) = self.tl_surface.get(key) {
                tl.with_pending_state(|st| {
                    st.states.set(xdg_toplevel::State::Maximized);
                    st.size = Some((work_w, work_h).into());

                    if *id == wid {
                        st.states.set(xdg_toplevel::State::Activated);
                    } else {
                        st.states.unset(xdg_toplevel::State::Activated);
                    }
                });
                tl.send_configure();
            }
        }

        // Seat keyboard focus (THIS makes typing work)
        if let Some(key) = self.wid_to_surface.get(&wid).cloned() {
            if let Some(tl) = self.tl_surface.get(&key) {
                if let Some(kbd) = self.seat.get_keyboard() {
                    let serial = SERIAL_COUNTER.next_serial();
                    kbd.set_focus(self, Some(tl.wl_surface().clone()), serial);
                }
            }
        }
    }
    
    fn apply_flow_action(&mut self, act: FlowAction) {
        match act {
            FlowAction::Focus(id) => self.set_focus(id),
            FlowAction::Close(id) => self.close_toplevel(id),
            FlowAction::None => {}
            _ => {}
        }
    }
    
    fn focus_next(&mut self) {
        let act = self.flow.handle(FlowEvent::FocusNext);
        self.apply_flow_action(act);
    }

    fn spawn_terminal(&self) {
        Command::new("weston-terminal")
            .env("WAYLAND_DISPLAY", &self.wayland_display)
            .env_remove("DISPLAY")
            .spawn()
            .ok();
    }

    fn handle_key_action(&mut self, action: KeyAction, running: &Rc<Cell<bool>>) {
        match action {
            KeyAction::ToggleLauncher => {
                self.launcher_open = !self.launcher_open;
            }
            KeyAction::QuitCompositor => {
                running.set(false);
            }
            KeyAction::CloseFocused => {
                let flow_action = self.flow.handle(FlowEvent::CloseFocused);
                self.apply_flow_action(flow_action);
            }
            
            KeyAction::FocusNext => {
                self.focus_next();
            }
            
            KeyAction::FocusPrev => {
                //self.focus_prev();
            }
            
            KeyAction::LaunchTerminal => {
                self.spawn_terminal();
            }
            
            KeyAction::OverflowView => {
               //self.open_overflow_view();
            }
            
            KeyAction::ActivateSlot(n) => {
               //self.activate_pin_slot(n);
            }
            
            KeyAction::AssignSlot(n) => {
               //self.pin_focused_to_slot(n);
            }
            
        }
    }
    
    fn forget_by_key(&mut self, key: &ObjectId) -> Option<WindowId> {
        let wid = self.surface_to_wid.remove(key);
        let _ = self.tl_surface.remove(key);

        if let Some(wid) = wid {
            self.wid_to_surface.remove(&wid);
        }
        wid
    }
    
    fn close_toplevel(&mut self, wid: WindowId) {
        flog_info!("close request wid={wid}");
        flog_info!("known wids={:?}", self.wid_to_surface.keys());

        let Some(key) = self.wid_to_surface.get(&wid).cloned() else {
            flog_info!("close_toplevel: unknown wid={wid}");
        return;
        };
        
        if let Some(tl) = self.tl_surface.get(&key) {
            tl.send_close();
        }
    }
    

    fn configure_toplevel_to_work_area(&self, tl: &ToplevelSurface) {
        // ✅ Logical size clients expect, and it matches your bar/layout math
        let sz = self.layout.client_size(); // Size<i32, Logical>
        let work_w = sz.w.max(1);
        let work_h = sz.h.max(1);

        flog_info!("cfg client_size (logical) {}x{} sf={}", work_w, work_h, self.scale_factor);

        tl.with_pending_state(|st| {
            st.states.set(xdg_toplevel::State::Maximized);
            st.size = Some((work_w, work_h).into());
        });
        tl.send_configure();
    }
    
    fn configure_all_to_work_area(&mut self) {
        for tl in self.tl_surface.values() {
            self.configure_toplevel_to_work_area(tl);
        }
    }
}

impl SelectionHandler for App {
    type SelectionUserData = ();
}

impl OutputHandler for App {
    
}

impl DataDeviceHandler for App {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

impl WaylandDndGrabHandler for App {}

impl CompositorHandler for App {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
    }
        

    fn destroyed(&mut self, surface: &WlSurface) {
        let key: ObjectId = surface.id();
        let wid = self.surface_to_wid.remove(&key);

        if let Some(wid) = wid {
            self.wid_to_surface.remove(&wid);
            self.tl_window.remove(&wid);
        }
        self.tl_surface.remove(&key);
    }
}    
    


impl ShmHandler for App {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl SeatHandler for App {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&WlSurface>) {}
    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: smithay::input::pointer::CursorImageStatus) {}
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    struct ModMask: u32 {
        const SHIFT = 0b0001;
        const CTRL  = 0b0010;
        const ALT   = 0b0100;
        const SUPER = 0b1000;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct KeyCombo {
    mods: ModMask,
    sym: u32,
}


pub struct App {
    display: Display::<App>,
    dh: DisplayHandle,
    chrome: Chrome,
    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    shm_state: ShmState,
    seat_state: SeatState<Self>,
    data_device_state: DataDeviceState,
    tl_surface: HashMap<ObjectId, ToplevelSurface>,
    tl_window: HashMap<WindowId, Window>,
    space: Space<Window>,
    window_output: HashMap<WindowId, OutputId>,   // where the window “lives”
    active_output: OutputId,
    surface_to_wid: HashMap<ObjectId, WindowId>,
    wid_to_surface: HashMap<WindowId, ObjectId>,
    pub seat: Seat<Self>,
    flow: FocalDesk,
    
    
    
    output_size: Size<i32, smithay::utils::Physical>,
    
    //focused: Option<WindowId>,
    launcher_open: bool,
    keybinds: HashMap<KeyCombo, KeyAction>,
    wayland_display: String,
    scale_factor: f64,
    buffer_scale: i32,
    layout_engine: LayoutEngine,
    layout: LayoutSnapshot,
    output_manager_state: OutputManagerState,
    output: Option<smithay::output::Output>,
    wallpaper_texture: Option<GlesTexture>,
    scratch_damage: [Rectangle<i32, Physical>; 8],
    scratch_damage_len: usize,
    text: crate::text::TextSystem,
    clock: ClockCache,
    pub needs_redraw: bool,
    mapped: HashSet<WindowId>,
    mapped_surfaces: HashSet<ObjectId>,
    pub dh:DisplayHandle,
}






pub fn send_frames_surface_tree(surface: &wl_surface::WlSurface, time: u32) {
    with_surface_tree_downward(
        surface,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |_surf, states, &()| {
            // the surface may not have any user_data if it is a subsurface and has not
            // yet been commited
            for callback in states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .frame_callbacks
                .drain(..)
            {
                callback.done(time);
            }
        },
        |_, _, &()| true,
    );
}

#[derive(Default)]
pub struct ClientState {
    compositor_state: CompositorClientState,
}
impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {
        flog_info!("initialized");
    }

    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {
        flog_info!("disconnected");
    }
}

// Macros used to delegate protocol handling to types in the app state.delegate_xdg_shell!(App);
delegate_output!(App);
delegate_compositor!(App);
delegate_shm!(App);
delegate_seat!(App);
delegate_data_device!(App);
delegate_xdg_shell!(App);
