//! Synthetic metric outcomes; does not call a model or touch documents.
use focaldesk_logging::{
    metrics::{Operation, Outcome, Stage},
    telemetry,
};
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let _guard = telemetry::init("focaldesk-metrics-smoke")?;
    for outcome in [
        Outcome::Ok,
        Outcome::Error,
        Outcome::Timeout,
        Outcome::Cancelled,
    ] {
        let mut operation = Operation::start(Stage::Request);
        operation.finish(outcome);
    }
    Ok(())
}
