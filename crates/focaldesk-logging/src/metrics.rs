//! Unsampled, bounded-cardinality operation metrics. Never accepts user labels.
use opentelemetry::{
    metrics::{Counter, Histogram, Meter},
    KeyValue,
};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Request,
    Embedding,
    Inference,
    VectorSearch,
}
impl Stage {
    fn label(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Embedding => "embedding",
            Self::Inference => "inference",
            Self::VectorSearch => "vector_search",
        }
    }
}
#[derive(Clone, Copy)]
pub enum Outcome {
    Ok,
    Error,
    Timeout,
    Cancelled,
}
impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
        }
    }
}

const STAGES: [Stage; 4] = [
    Stage::Request,
    Stage::Embedding,
    Stage::Inference,
    Stage::VectorSearch,
];

#[derive(Default)]
struct LiveState {
    next: AtomicU64,
    active: Mutex<BTreeMap<u64, (Stage, Instant)>>,
}
impl LiveState {
    fn start(&self, stage: Stage, started: Instant) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.active
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id, (stage, started));
        id
    }
    fn snapshot(&self) -> Vec<(Stage, u64, f64)> {
        let active = self.active.lock().unwrap_or_else(|p| p.into_inner());
        STAGES
            .into_iter()
            .map(|stage| {
                let starts: Vec<_> = active
                    .values()
                    .filter(|(s, _)| *s == stage)
                    .map(|(_, t)| t)
                    .collect();
                let oldest = starts
                    .iter()
                    .map(|t| t.elapsed().as_secs_f64())
                    .fold(0.0, f64::max);
                (stage, starts.len() as u64, oldest)
            })
            .collect()
    }
}

#[derive(Clone)]
struct Instruments {
    requests: Counter<u64>,
    duration: Histogram<f64>,
    live: Arc<LiveState>,
    first_content: Histogram<f64>,
    output_tokens: Counter<u64>,
    stream_duration: Histogram<f64>,
    ollama_phase: Histogram<f64>,
    ollama_tokens: Counter<u64>,
    ollama_decode_seconds: Counter<f64>,
}
impl Instruments {
    fn new(meter: Meter) -> Self {
        let live = Arc::new(LiveState::default());
        meter
            .f64_observable_gauge("focaldesk.service.heartbeat")
            .with_description(
                "Unix timestamp of the latest SDK metric collection; detects stale exports",
            )
            .with_unit("s")
            .with_callback(|observer| {
                observer.observe(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs_f64(),
                    &[],
                );
            })
            .build();
        let active = live.clone();
        meter
            .u64_observable_gauge("focaldesk.operation.active")
            .with_description("Currently running operations by stage")
            .with_callback(move |observer| {
                for (stage, count, _) in active.snapshot() {
                    observer.observe(count, &[KeyValue::new("stage", stage.label())]);
                }
            })
            .build();
        let oldest = live.clone();
        meter
            .f64_observable_gauge("focaldesk.operation.oldest")
            .with_description("Age of the oldest active operation; zero when idle")
            .with_unit("s")
            .with_callback(move |observer| {
                for (stage, _, age) in oldest.snapshot() {
                    observer.observe(age, &[KeyValue::new("stage", stage.label())]);
                }
            })
            .build();
        Self {
            ollama_phase: meter
                .f64_histogram("focaldesk.ollama.phase.duration")
                .with_description(
                    "Ollama-reported duration per completed provider attempt, not client wall time",
                )
                .with_unit("s")
                .with_boundaries(vec![
                    0.0001, 0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
                    15.0, 30.0, 60.0, 120.0,
                ])
                .build(),
            ollama_tokens: meter
                .u64_counter("focaldesk.ollama.generated_tokens")
                .with_description(
                    "Ollama-reported generated tokens with a positive reported generation duration",
                )
                .build(),
            ollama_decode_seconds: meter
                .f64_counter("focaldesk.ollama.generation_time")
                .with_description(
                    "Ollama-reported generation time paired with generated-token counts",
                )
                .with_unit("s")
                .build(),
            live,
            first_content: meter
                .f64_histogram("focaldesk.inference.first_content")
                .with_description(
                    "Streaming time to first nonempty content chunk, including startup and retries",
                )
                .with_unit("s")
                .with_boundaries(vec![
                    0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0,
                ])
                .build(),
            output_tokens: meter
                .u64_counter("focaldesk.inference.output_tokens")
                .with_description(
                    "Provider-reported output tokens for completed successful streams",
                )
                .build(),
            stream_duration: meter
                .f64_histogram("focaldesk.inference.stream_duration")
                .with_description(
                    "End-to-end duration of successful streams with reported output tokens",
                )
                .with_unit("s")
                .with_boundaries(vec![
                    0.1, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0,
                ])
                .build(),
            requests: meter
                .u64_counter("focaldesk.operation.requests")
                .with_description("Completed operations by stage and outcome")
                .build(),
            duration: meter
                .f64_histogram("focaldesk.operation.duration")
                .with_description("Operation wall time including retries, in seconds")
                .with_unit("s")
                .with_boundaries(vec![
                    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
                    15.0, 30.0, 60.0, 90.0, 120.0, 180.0, 300.0,
                ])
                .build(),
        }
    }
    fn record(&self, stage: Stage, outcome: Outcome, seconds: f64) {
        let labels = [
            KeyValue::new("stage", stage.label()),
            KeyValue::new("outcome", outcome.label()),
        ];
        self.requests.add(1, &labels);
        self.duration.record(seconds, &labels);
    }
}
static INSTRUMENTS: OnceLock<Instruments> = OnceLock::new();
pub(crate) fn install(meter: Meter) {
    let instruments = Instruments::new(meter);
    // A real zero baseline allows the first completed operation to appear in
    // selected-range increases. Do not seed histogram samples with fake zeros.
    for stage in STAGES {
        for outcome in [
            Outcome::Ok,
            Outcome::Error,
            Outcome::Timeout,
            Outcome::Cancelled,
        ] {
            instruments.requests.add(
                0,
                &[
                    KeyValue::new("stage", stage.label()),
                    KeyValue::new("outcome", outcome.label()),
                ],
            );
        }
    }
    let _ = INSTRUMENTS.set(instruments);
}

/// Counts once when dropped, including task cancellation. Explicit outcomes win
/// over outer success/error wrappers (a cancelled stream may return Ok).
pub struct Operation {
    instruments: Option<Instruments>,
    stage: Stage,
    started: Instant,
    outcome: Option<Outcome>,
    live_id: Option<u64>,
}
impl Operation {
    pub fn start(stage: Stage) -> Self {
        Self::with_instruments(INSTRUMENTS.get().cloned(), stage)
    }
    fn with_instruments(instruments: Option<Instruments>, stage: Stage) -> Self {
        let started = Instant::now();
        let live_id = instruments.as_ref().map(|i| i.live.start(stage, started));
        Self {
            instruments,
            stage,
            started,
            outcome: None,
            live_id,
        }
    }
    pub fn finish(&mut self, outcome: Outcome) {
        if self.outcome.is_none() {
            self.outcome = Some(outcome);
        }
    }
}
impl Drop for Operation {
    fn drop(&mut self) {
        if let Some(instruments) = &self.instruments {
            if let Some(id) = self.live_id {
                instruments
                    .live
                    .active
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&id);
            }
            instruments.record(
                self.stage,
                self.outcome.unwrap_or(Outcome::Cancelled),
                self.started.elapsed().as_secs_f64(),
            );
        }
    }
}

/// Numeric timings from a completed Ollama attempt. Missing fields are omitted;
/// reported zero durations are real measurements (for example cached prefill).
pub fn record_ollama_timings(
    load: Option<u64>,
    prompt: Option<u64>,
    generation: Option<u64>,
    tokens: Option<u64>,
) {
    if let Some(instruments) = INSTRUMENTS.get() {
        instruments.record_ollama(load, prompt, generation, tokens);
    }
    let span = tracing::Span::current();
    for (field, value) in [
        ("ollama.model_load_seconds", load),
        ("ollama.prompt_eval_seconds", prompt),
        ("ollama.generation_seconds", generation),
    ] {
        if let Some(nanos) = value {
            span.record(field, nanos as f64 / 1e9);
        }
    }
    if let Some(tokens) = tokens {
        span.record("ollama.output_tokens", tokens);
    }
}
impl Instruments {
    fn record_ollama(
        &self,
        load: Option<u64>,
        prompt: Option<u64>,
        generation: Option<u64>,
        tokens: Option<u64>,
    ) {
        for (phase, duration) in [
            ("model_load", load),
            ("prompt_eval", prompt),
            ("generation", generation),
        ] {
            if let Some(nanos) = duration {
                self.ollama_phase
                    .record(nanos as f64 / 1e9, &[KeyValue::new("phase", phase)]);
            }
        }
        if let (Some(nanos), Some(tokens)) =
            (generation.filter(|n| *n > 0), tokens.filter(|n| *n > 0))
        {
            self.ollama_tokens.add(tokens, &[]);
            self.ollama_decode_seconds.add(nanos as f64 / 1e9, &[]);
        }
    }
}

/// Measures streaming responsiveness without inspecting or retaining output text.
/// Output throughput uses provider token usage and full stream wall time.
pub struct Stream {
    instruments: Option<Instruments>,
    started: Instant,
    first: bool,
    finished: bool,
}
impl Stream {
    pub fn start() -> Self {
        Self {
            instruments: INSTRUMENTS.get().cloned(),
            started: Instant::now(),
            first: false,
            finished: false,
        }
    }
    pub fn content(&mut self, nonempty: bool) {
        if nonempty && !self.first {
            self.first = true;
            if let Some(i) = &self.instruments {
                i.first_content
                    .record(self.started.elapsed().as_secs_f64(), &[]);
            }
        }
    }
    pub fn finish(&mut self, output_tokens: Option<u64>) {
        if self.finished {
            return;
        }
        self.finished = true;
        if let (Some(i), Some(tokens)) = (&self.instruments, output_tokens.filter(|n| *n > 0)) {
            i.output_tokens.add(tokens, &[]);
            i.stream_duration
                .record(self.started.elapsed().as_secs_f64(), &[]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::metrics::MeterProvider;
    use opentelemetry_sdk::metrics::{
        data::{AggregatedMetrics, MetricData},
        InMemoryMetricExporter, PeriodicReader, SdkMeterProvider,
    };
    #[test]
    fn records_exactly_once_with_fixed_labels_and_seconds_buckets() {
        let exporter = InMemoryMetricExporter::default();
        let provider = SdkMeterProvider::builder()
            .with_reader(PeriodicReader::builder(exporter.clone()).build())
            .build();
        let instruments = Instruments::new(provider.meter("test"));
        for outcome in [
            Some(Outcome::Ok),
            Some(Outcome::Error),
            Some(Outcome::Timeout),
            None,
        ] {
            let mut op = Operation::with_instruments(Some(instruments.clone()), Stage::Request);
            if let Some(outcome) = outcome {
                op.finish(outcome);
                op.finish(Outcome::Ok);
            }
        }
        provider.force_flush().unwrap();
        let exported = exporter.get_finished_metrics().unwrap();
        let metrics: Vec<_> = exported
            .iter()
            .flat_map(|r| r.scope_metrics())
            .flat_map(|s| s.metrics())
            .collect();
        let counter = metrics
            .iter()
            .find(|m| m.name() == "focaldesk.operation.requests")
            .unwrap();
        let AggregatedMetrics::U64(MetricData::Sum(sum)) = counter.data() else {
            panic!("expected counter")
        };
        assert_eq!(sum.data_points().map(|p| p.value()).sum::<u64>(), 4);
        assert_eq!(sum.data_points().count(), 4);
        for point in sum.data_points() {
            assert_eq!(point.value(), 1);
            assert_eq!(point.attributes().count(), 2);
            assert!(point
                .attributes()
                .all(|kv| ["stage", "outcome"].contains(&kv.key.as_str())));
        }
        let histogram = metrics
            .iter()
            .find(|m| m.name() == "focaldesk.operation.duration")
            .unwrap();
        assert_eq!(histogram.unit(), "s");
        let AggregatedMetrics::F64(MetricData::Histogram(hist)) = histogram.data() else {
            panic!("expected histogram")
        };
        assert_eq!(hist.data_points().map(|p| p.count()).sum::<u64>(), 4);
        assert!(hist.data_points().all(|p| p.bounds().next() == Some(0.001)));
        provider.shutdown().unwrap();
    }
    #[test]
    fn live_gauges_track_active_work_and_clear_after_cancellation() {
        let exporter = InMemoryMetricExporter::default();
        let provider = SdkMeterProvider::builder()
            .with_reader(PeriodicReader::builder(exporter.clone()).build())
            .build();
        let instruments = Instruments::new(provider.meter("live-test"));
        let operation = Operation::with_instruments(Some(instruments.clone()), Stage::Request);
        let snapshot = instruments.live.snapshot();
        assert!(snapshot
            .iter()
            .any(|(stage, count, age)| *stage == Stage::Request && *count == 1 && *age >= 0.0));
        provider.force_flush().unwrap();
        let exported = exporter.get_finished_metrics().unwrap();
        let metrics: Vec<_> = exported
            .iter()
            .flat_map(|r| r.scope_metrics())
            .flat_map(|s| s.metrics())
            .collect();
        let heartbeat = metrics
            .iter()
            .find(|m| m.name() == "focaldesk.service.heartbeat")
            .unwrap();
        let AggregatedMetrics::F64(MetricData::Gauge(gauge)) = heartbeat.data() else {
            panic!("expected heartbeat")
        };
        assert!(gauge.data_points().all(|p| p.value() > 1_700_000_000.0));
        let active = metrics
            .iter()
            .find(|m| m.name() == "focaldesk.operation.active")
            .unwrap();
        let AggregatedMetrics::U64(MetricData::Gauge(gauge)) = active.data() else {
            panic!("expected active gauge")
        };
        assert_eq!(gauge.data_points().map(|p| p.value()).sum::<u64>(), 1);
        drop(operation);
        assert!(instruments
            .live
            .snapshot()
            .iter()
            .all(|(_, count, age)| *count == 0 && *age == 0.0));
        provider.shutdown().unwrap();
    }

    #[test]
    fn stream_records_first_content_once_and_only_reported_tokens() {
        let exporter = InMemoryMetricExporter::default();
        let provider = SdkMeterProvider::builder()
            .with_reader(PeriodicReader::builder(exporter.clone()).build())
            .build();
        let instruments = Instruments::new(provider.meter("stream-test"));
        let mut stream = Stream {
            instruments: Some(instruments.clone()),
            started: Instant::now(),
            first: false,
            finished: false,
        };
        stream.content(false);
        assert!(!stream.first);
        stream.content(true);
        stream.content(true);
        stream.finish(Some(12));
        stream.finish(Some(200));
        let mut unknown = Stream {
            instruments: Some(instruments),
            started: Instant::now(),
            first: false,
            finished: false,
        };
        unknown.finish(None);
        provider.force_flush().unwrap();
        let exported = exporter.get_finished_metrics().unwrap();
        let metrics: Vec<_> = exported
            .iter()
            .flat_map(|r| r.scope_metrics())
            .flat_map(|s| s.metrics())
            .collect();
        let token_metric = metrics
            .iter()
            .find(|m| m.name() == "focaldesk.inference.output_tokens")
            .unwrap();
        let AggregatedMetrics::U64(MetricData::Sum(sum)) = token_metric.data() else {
            panic!("expected tokens")
        };
        assert_eq!(sum.data_points().map(|p| p.value()).sum::<u64>(), 12);
        for name in [
            "focaldesk.inference.first_content",
            "focaldesk.inference.stream_duration",
        ] {
            let metric = metrics.iter().find(|m| m.name() == name).unwrap();
            let AggregatedMetrics::F64(MetricData::Histogram(hist)) = metric.data() else {
                panic!("expected histogram")
            };
            assert_eq!(hist.data_points().map(|p| p.count()).sum::<u64>(), 1);
        }
        provider.shutdown().unwrap();
    }
    #[test]
    fn ollama_phases_use_seconds_keep_real_zeros_and_pair_tokens_with_decode_time() {
        let exporter = InMemoryMetricExporter::default();
        let provider = SdkMeterProvider::builder()
            .with_reader(PeriodicReader::builder(exporter.clone()).build())
            .build();
        let instruments = Instruments::new(provider.meter("ollama-phase-test"));
        instruments.record_ollama(Some(0), Some(100_000_000), Some(500_000_000), Some(50));
        instruments.record_ollama(None, Some(0), Some(0), Some(20));
        instruments.record_ollama(None, None, None, None);
        provider.force_flush().unwrap();
        let exported = exporter.get_finished_metrics().unwrap();
        let metrics: Vec<_> = exported
            .iter()
            .flat_map(|r| r.scope_metrics())
            .flat_map(|s| s.metrics())
            .collect();
        let phase = metrics
            .iter()
            .find(|m| m.name() == "focaldesk.ollama.phase.duration")
            .unwrap();
        assert_eq!(phase.unit(), "s");
        let AggregatedMetrics::F64(MetricData::Histogram(hist)) = phase.data() else {
            panic!("expected phases")
        };
        assert_eq!(hist.data_points().count(), 3);
        for point in hist.data_points() {
            assert_eq!(point.attributes().count(), 1);
            let phase = point.attributes().next().unwrap();
            assert_eq!(phase.key.as_str(), "phase");
            match phase.value.as_str().as_ref() {
                "model_load" => {
                    assert_eq!(point.count(), 1);
                    assert_eq!(point.sum(), 0.0);
                }
                "prompt_eval" => {
                    assert_eq!(point.count(), 2);
                    assert!((point.sum() - 0.1).abs() < 1e-9);
                }
                "generation" => {
                    assert_eq!(point.count(), 2);
                    assert!((point.sum() - 0.5).abs() < 1e-9);
                }
                _ => panic!("unexpected phase"),
            }
        }
        let tokens = metrics
            .iter()
            .find(|m| m.name() == "focaldesk.ollama.generated_tokens")
            .unwrap();
        let AggregatedMetrics::U64(MetricData::Sum(sum)) = tokens.data() else {
            panic!("expected tokens")
        };
        assert_eq!(sum.data_points().map(|p| p.value()).sum::<u64>(), 50);
        let seconds = metrics
            .iter()
            .find(|m| m.name() == "focaldesk.ollama.generation_time")
            .unwrap();
        let AggregatedMetrics::F64(MetricData::Sum(sum)) = seconds.data() else {
            panic!("expected seconds")
        };
        assert!((sum.data_points().map(|p| p.value()).sum::<f64>() - 0.5).abs() < 1e-9);
        provider.shutdown().unwrap();
    }
}
