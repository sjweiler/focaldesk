pub mod logging;

pub use logging::{
    current_log_level, enabled, init_default_logging, init_logging, init_logging_from_env,
    set_log_level, BuildMode, FLogLevel,
};

pub fn flog(msg: impl AsRef<str>) {
    logging::flog(FLogLevel::Info, msg);
}
