pub mod backend;
pub mod manager;
pub mod model;

pub use backend::{UpdateBackendKind, detect_backend};
pub use manager::UpdateManager;
pub use model::{UpdatePackage, UpdateSnapshot};
