#[path = "../error.rs"]
pub mod error;
pub mod identity;
#[path = "../manager.rs"]
pub mod manager;
#[path = "../policy.rs"]
pub mod policy;
#[path = "../prompt.rs"]
pub mod prompt;
#[path = "../request.rs"]
pub mod request;
#[path = "../session.rs"]
pub mod session;
#[path = "../store.rs"]
pub mod store;
#[path = "../types.rs"]
pub mod types;

pub use manager::PermissionManager;
pub use identity::AppIdentity;
pub use request::{PermissionRequest, PermissionResource};
pub use types::{PermissionDecision, PermissionScope, PermissionState};
pub use session::{GrantToken, ActiveGrant};
