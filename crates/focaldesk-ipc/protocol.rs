// ipc protocol stuff goes here

// crates/compositor/src/ipc/protocol.rs

use serde::{Deserialize, Serialize};

/// Version your protocol from day one.
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub version: u32,
    pub request: Request,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    pub version: u32,
    pub response: Response,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    Ping,

    GetState,

    FocusSlot {
        slot: u8,
    },

    Launch {
        command: String,
    },

    AssignSlot {
        slot: u8,
        app_id: String,
    },

    OpenLauncher,

    CloseLauncher,

    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Pong,

    Ok,

    State(StateSnapshot),

    Error {
        code: ErrorCode,
        message: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StateSnapshot {
    pub active_slot: u8,
    pub launcher_open: bool,
    pub active_output: u8,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ErrorCode {
    InvalidRequest,
    Unauthorized,
    InvalidSlot,
    InternalError,
}
