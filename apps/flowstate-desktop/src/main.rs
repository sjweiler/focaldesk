use anyhow::Result;

#[cfg(feature = "drm")]
use flowstate_engine::backend::drm;

#[cfg(feature = "winit")]
use flowstate_engine::backend::winit;

#[cfg(feature = "drm")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    drm::run()
}

#[cfg(all(not(feature = "drm"), feature = "winit"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    winit::run()
}
