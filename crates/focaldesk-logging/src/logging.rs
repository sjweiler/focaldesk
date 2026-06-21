use std::backtrace::Backtrace;
use std::fs::{create_dir_all, File, OpenOptions};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

use tracing_journald::{Layer as JournaldLayer, Priority, PriorityMappings};
use tracing_log::LogTracer;
use tracing_subscriber::fmt;
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FLogLevel {
    Critical = 0,
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
    Trace = 5,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildMode {
    Dev,
    Production,
}

static LOG_LEVEL: AtomicU8 = AtomicU8::new(FLogLevel::Info as u8);
static TRACING_INSTALLED: OnceLock<()> = OnceLock::new();
static LOG_GUARD: OnceLock<Mutex<Option<tracing_appender::non_blocking::WorkerGuard>>> =
    OnceLock::new();
static PANIC_HOOK_INSTALLED: OnceLock<()> = OnceLock::new();

pub fn session_id() -> u32 {
    std::process::id()
}

pub fn log_file_path_candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Ok(path) = std::env::var("FOCALDESK_LOG_FILE") {
        paths.push(PathBuf::from(path));
    }

    if let Some(state_dir) = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from) {
        paths.push(state_dir.join("focaldesk").join("focaldesk.log"));
    }

    if let Some(cache_dir) = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".cache"))
        })
    {
        paths.push(cache_dir.join("focaldesk").join("focaldesk.log"));
    }

    paths.push(PathBuf::from("/tmp/focaldesk.log"));
    paths
}

fn open_log_file() -> Option<File> {
    for path in log_file_path_candidates() {
        if let Some(parent) = path.parent() {
            let _ = create_dir_all(parent);
        }

        if let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) {
            return Some(file);
        }
    }

    None
}

fn make_writer() -> BoxMakeWriter {
    if let Some(file) = open_log_file() {
        let (writer, guard) = tracing_appender::non_blocking(file);
        let slot = LOG_GUARD.get_or_init(|| Mutex::new(None));
        if let Ok(mut slot) = slot.lock() {
            *slot = Some(guard);
        }
        BoxMakeWriter::new(writer)
    } else {
        BoxMakeWriter::new(std::io::stderr)
    }
}

#[cfg(target_os = "linux")]
fn install_tracing_subscriber() {
    TRACING_INSTALLED.get_or_init(|| {
        let _ = LogTracer::init();
        let filter = EnvFilter::new("trace");
        let writer = make_writer();
        match JournaldLayer::new() {
            Ok(layer) => {
                let layer = layer
                    .with_syslog_identifier("focaldesk".to_string())
                    .with_priority_mappings(PriorityMappings {
                        error: Priority::Error,
                        warn: Priority::Warning,
                        info: Priority::Notice,
                        debug: Priority::Informational,
                        trace: Priority::Debug,
                    });
                let subscriber = tracing_subscriber::registry()
                    .with(filter)
                    .with(fmt::layer().with_writer(writer).with_ansi(false))
                    .with(layer);
                let _ = tracing::subscriber::set_global_default(subscriber);
            }
            Err(err) => {
                eprintln!("focaldesk: journald logging unavailable: {err}");
                let subscriber = tracing_subscriber::registry()
                    .with(filter)
                    .with(fmt::layer().with_writer(writer).with_ansi(false));
                let _ = tracing::subscriber::set_global_default(subscriber);
            }
        }
    });
}

#[cfg(not(target_os = "linux"))]
fn install_tracing_subscriber() {
    TRACING_INSTALLED.get_or_init(|| {
        let _ = LogTracer::init();
        let filter = EnvFilter::new("trace");
        let writer = make_writer();
        let subscriber = tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().with_writer(writer).with_ansi(false));
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
}

pub fn set_log_level(level: FLogLevel) {
    LOG_LEVEL.store(level as u8, Ordering::Relaxed);
}

pub fn current_log_level() -> FLogLevel {
    match LOG_LEVEL.load(Ordering::Relaxed) {
        0 => FLogLevel::Critical,
        1 => FLogLevel::Error,
        2 => FLogLevel::Warn,
        3 => FLogLevel::Info,
        4 => FLogLevel::Debug,
        5 => FLogLevel::Trace,
        _ => FLogLevel::Info,
    }
}

pub fn enabled(level: FLogLevel) -> bool {
    (level as u8) <= LOG_LEVEL.load(Ordering::Relaxed)
}

pub fn flog(level: FLogLevel, msg: impl AsRef<str>) {
    if !enabled(level) {
        return;
    }

    let line = format!("[{:?}] {}", level, msg.as_ref());
    match level {
        FLogLevel::Critical | FLogLevel::Error => {
            tracing::error!(target: "focaldesk", "{line}");
        }
        FLogLevel::Warn => {
            tracing::warn!(target: "focaldesk", "{line}");
        }
        FLogLevel::Info => {
            tracing::info!(target: "focaldesk", "{line}");
        }
        FLogLevel::Debug => {
            tracing::debug!(target: "focaldesk", "{line}");
        }
        FLogLevel::Trace => {
            tracing::trace!(target: "focaldesk", "{line}");
        }
    }
}

pub fn init_logging(mode: BuildMode) {
    match mode {
        BuildMode::Dev => set_log_level(FLogLevel::Debug),
        BuildMode::Production => set_log_level(FLogLevel::Warn),
    }
}

pub fn init_logging_from_env(default_mode: BuildMode) {
    init_logging(default_mode);

    let Ok(level) = std::env::var("FOCALDESK_LOG") else {
        return;
    };

    let level = match level.to_lowercase().as_str() {
        "critical" => FLogLevel::Critical,
        "error" => FLogLevel::Error,
        "warn" | "warning" => FLogLevel::Warn,
        "info" => FLogLevel::Info,
        "debug" => FLogLevel::Debug,
        "trace" => FLogLevel::Trace,
        _ => current_log_level(),
    };

    set_log_level(level);
}

pub fn init_default_logging() {
    #[cfg(debug_assertions)]
    init_logging_from_env(BuildMode::Dev);

    #[cfg(not(debug_assertions))]
    init_logging_from_env(BuildMode::Production);

    install_tracing_subscriber();
    install_panic_hook();
}

pub fn startup_banner(app_name: &str, version: &str, backend: &str) {
    tracing::info!(
        target: "focaldesk",
        session_id = session_id(),
        app = app_name,
        version = version,
        backend = backend,
        profile = if cfg!(debug_assertions) { "debug" } else { "release" },
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        "startup"
    );
}

pub fn install_panic_hook() {
    PANIC_HOOK_INSTALLED.get_or_init(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            let location = panic_info
                .location()
                .map(|location| {
                    format!(
                        "{}:{}:{}",
                        location.file(),
                        location.line(),
                        location.column()
                    )
                })
                .unwrap_or_else(|| "unknown location".to_string());

            let payload = if let Some(msg) = panic_info.payload().downcast_ref::<&str>() {
                (*msg).to_string()
            } else if let Some(msg) = panic_info.payload().downcast_ref::<String>() {
                msg.clone()
            } else {
                "non-string panic payload".to_string()
            };

            let message = format!("panic captured at {location}: {payload}");
            let backtrace = Backtrace::force_capture();
            eprintln!("{message}");
            eprintln!("panic backtrace:\n{:?}", backtrace);
            tracing::error!(target: "focaldesk", "{message}");
            tracing::error!(target: "focaldesk", "panic backtrace:\n{:?}", backtrace);

            default_hook(panic_info);
        }));
    });
}

#[macro_export]
macro_rules! flog_critical {
    ($($arg:tt)*) => {
        $crate::logging::flog(
            $crate::logging::FLogLevel::Critical,
            format!($($arg)*)
        )
    };
}

#[macro_export]
macro_rules! flog_error {
    ($($arg:tt)*) => {
        $crate::logging::flog(
            $crate::logging::FLogLevel::Error,
            format!($($arg)*)
        )
    };
}

#[macro_export]
macro_rules! flog_warn {
    ($($arg:tt)*) => {
        $crate::logging::flog(
            $crate::logging::FLogLevel::Warn,
            format!($($arg)*)
        )
    };
}

#[macro_export]
macro_rules! flog_info {
    ($($arg:tt)*) => {
        $crate::logging::flog(
            $crate::logging::FLogLevel::Info,
            format!($($arg)*)
        )
    };
}

#[macro_export]
macro_rules! flog_debug {
    ($($arg:tt)*) => {
        $crate::logging::flog(
            $crate::logging::FLogLevel::Debug,
            format!($($arg)*)
        )
    };
}

#[macro_export]
macro_rules! flog_trace {
    ($($arg:tt)*) => {
        $crate::logging::flog(
            $crate::logging::FLogLevel::Trace,
            format!($($arg)*)
        )
    };
}
