//! Opt-in traces and unsampled operation metrics for the AI request path.
use opentelemetry::{
    propagation::TextMapPropagator,
    trace::{TraceContextExt, TracerProvider},
};
use opentelemetry_sdk::{propagation::TraceContextPropagator, trace::SdkTracerProvider, Resource};
use std::collections::HashMap;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, Layer};

/// Keep alive until process exit so pending spans are flushed on normal shutdown.
pub struct TelemetryGuard(
    Option<SdkTracerProvider>,
    Option<opentelemetry_sdk::metrics::SdkMeterProvider>,
);
impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(provider) = &self.1 {
            let _ = provider.shutdown();
        }
        if let Some(provider) = &self.0 {
            let _ = provider.shutdown();
        }
    }
}

/// Export only explicitly reviewed spans. Existing diagnostic events can contain
/// user content and are deliberately excluded from the OTLP pipeline.
pub fn init(service: &str) -> Result<TelemetryGuard, Box<dyn std::error::Error + Send + Sync>> {
    let provider = if std::env::var("FOCALDESK_OTEL").as_deref() == Ok("1") {
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .build()?;
        Some(
            SdkTracerProvider::builder()
                .with_batch_exporter(exporter)
                .with_resource(
                    Resource::builder()
                        .with_service_name(service.to_string())
                        .build(),
                )
                .build(),
        )
    } else {
        None
    };
    use opentelemetry::metrics::MeterProvider;
    let metrics = if provider.is_some() {
        let exporter = opentelemetry_otlp::MetricExporter::builder()
            .with_http()
            .build()?;
        let metrics = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
            .with_resource(
                Resource::builder()
                    .with_service_name(service.to_string())
                    .build(),
            )
            .with_periodic_exporter(exporter)
            .build();
        crate::metrics::install(metrics.meter("focaldesk"));
        Some(metrics)
    } else {
        None
    };
    let otel = provider.as_ref().map(|provider| {
        tracing_opentelemetry::layer()
            .with_tracer(provider.tracer("focaldesk"))
            .with_filter(tracing_subscriber::filter::filter_fn(|meta| {
                meta.is_span() && meta.target() == "focaldesk.telemetry"
            }))
    });
    tracing_subscriber::registry()
        .with(otel)
        .with(
            tracing_subscriber::fmt::layer().with_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            ),
        )
        .try_init()?;
    Ok(TelemetryGuard(provider, metrics))
}

pub fn inject_context() -> HashMap<String, String> {
    let mut carrier = HashMap::new();
    TraceContextPropagator::new().inject_context(&tracing::Span::current().context(), &mut carrier);
    carrier
}

pub fn set_parent(span: &tracing::Span, carrier: &HashMap<String, String>) {
    let parent = TraceContextPropagator::new().extract(carrier);
    let _ = span.set_parent(parent);
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::trace::TraceContextExt;
    use opentelemetry_sdk::trace::InMemorySpanExporter;

    #[test]
    fn propagates_remote_parent_and_excludes_diagnostic_content() {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let layer = tracing_opentelemetry::layer()
            .with_tracer(provider.tracer("test"))
            .with_filter(tracing_subscriber::filter::filter_fn(|meta| {
                meta.is_span() && meta.target() == "focaldesk.telemetry"
            }));
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            let root = tracing::info_span!(target: "focaldesk.telemetry", "client");
            let carrier = root.in_scope(inject_context);
            assert!(carrier.contains_key("traceparent"));
            let remote = TraceContextPropagator::new().extract(&carrier);
            assert!(remote.span().span_context().is_remote());
            let server = tracing::info_span!(target: "focaldesk.telemetry", "server");
            set_parent(&server, &carrier);
            server.in_scope(|| {
                tracing::info!(prompt = "PRIVATE_PROMPT", "PRIVATE_DOCUMENT");
                let _private = tracing::info_span!("diagnostic", secret = "PRIVATE_KEY").entered();
                let _query =
                    tracing::info_span!(target: "focaldesk.telemetry", "query", dimensions = 768)
                        .entered();
            });
        });
        provider.force_flush().unwrap();
        let spans = exporter.get_finished_spans().unwrap();
        assert_eq!(spans.len(), 3);
        let client = spans.iter().find(|s| s.name == "client").unwrap();
        let server = spans.iter().find(|s| s.name == "server").unwrap();
        let query = spans.iter().find(|s| s.name == "query").unwrap();
        assert_eq!(server.parent_span_id, client.span_context.span_id());
        assert_eq!(query.parent_span_id, server.span_context.span_id());
        assert_eq!(
            query.span_context.trace_id(),
            client.span_context.trace_id()
        );
        assert!(spans.iter().all(|span| span.events.is_empty()));
        assert!(!format!("{spans:?}").contains("PRIVATE_"));
        provider.shutdown().unwrap();
    }
}

/// Add IDs to local request log context without exporting diagnostic messages.
pub fn record_correlation(span: &tracing::Span) {
    let context = span.context();
    let trace = context.span();
    let ids = trace.span_context();
    if ids.is_valid() {
        span.record("trace_id", ids.trace_id().to_string());
        span.record("span_id", ids.span_id().to_string());
    }
}
