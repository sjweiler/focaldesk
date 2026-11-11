use focaldesk_types::OutputId;

/// Lifecycle events emitted by a compositor backend rather than an input device.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BackendEvent {
    OutputResized {
        output_id: OutputId,
        width: u32,
        height: u32,
        scale_factor: f64,
    },
    CloseRequested,
}
