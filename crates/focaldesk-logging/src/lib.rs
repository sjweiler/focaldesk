pub mod logging;

pub use logging::{
    crash_report_path_candidates, current_log_level, enabled, init_default_logging, init_logging,
    init_logging_from_env, install_panic_hook, log_file_path_candidates, session_id, set_log_level,
    startup_banner, BuildMode, FLogLevel,
};

pub fn flog(msg: impl AsRef<str>) {
    logging::flog(FLogLevel::Info, msg);
}
