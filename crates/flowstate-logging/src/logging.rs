use std::sync::atomic::{AtomicU8, Ordering};

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

    eprintln!("[{:?}] {}", level, msg.as_ref());
}

pub fn init_logging(mode: BuildMode) {
    match mode {
        BuildMode::Dev => set_log_level(FLogLevel::Debug),
        BuildMode::Production => set_log_level(FLogLevel::Warn),
    }
}

pub fn init_logging_from_env(default_mode: BuildMode) {
    init_logging(default_mode);

    let Ok(level) = std::env::var("FOCUSSHELL_LOG") else {
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
