/// Rendering/input backend used by the compositor session.
///
/// This identifies a compositor architecture choice, not a keybinding or
/// application-flow concern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompositorBackend {
    Winit,
    Drm,
}
