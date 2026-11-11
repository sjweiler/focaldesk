//! Synthetic trace; no AI provider, documents, or permissions required.
use focaldesk_logging::telemetry;
use opentelemetry::trace::TraceContextExt;
use tracing_opentelemetry::OpenTelemetrySpanExt;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let _guard = telemetry::init("focaldesk-otel-smoke")?;
    let client = tracing::info_span!(target: "focaldesk.telemetry", "ai.request");
    println!(
        "trace_id={}",
        client.context().span().span_context().trace_id()
    );
    let carrier = client.in_scope(telemetry::inject_context);
    let server = tracing::info_span!(target: "focaldesk.telemetry", "ai_ipc_request");
    telemetry::set_parent(&server, &carrier);
    server.in_scope(|| {
        for name in ["embedding", "search", "inference"] {
            let stage =
                tracing::info_span!(target: "focaldesk.telemetry", "smoke.stage", stage = name);
            stage.in_scope(|| std::thread::sleep(std::time::Duration::from_millis(10)));
        }
    });
    Ok(())
}
