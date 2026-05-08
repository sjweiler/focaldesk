// event stuff goes here

use flowstate_flow::WindowId;

#[derive(Debug, Clone)]
pub enum Event {

    // input
    KeyPressed {
        keycode: u32,
        modifiers: smithay::input::keyboard::ModifiersState,
    },

    PointerMoved {
        x: f64,
        y: f64,
    },

    PointerButton {
        button: u32,
        pressed: bool,
    },

    // Wayland lifecycle
    WindowCreated {
        wid: WindowId,
    },

    WindowDestroyed {
        wid: WindowId,
    },

    // compositor/output
    OutputResized {
        width: i32,
        height: i32,
        scale: f64,
    },

    RedrawRequested,
}
