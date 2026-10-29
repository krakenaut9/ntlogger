//! A sink's own level cannot raise the logger's: `log`'s level stays the logger's, and the
//! sink takes no record above it.
//!
//! A file of its own, so that it runs in a process of its own: `log` takes one logger
//! per process. It never logs, since that would run kernel functions.

use log::{Level, LevelFilter, Metadata};
use ntlogger::{DebugLoggerOptions, NtLoggerBuilder};

#[test]
fn the_debuggers_own_level_cannot_raise_the_loggers() {
    NtLoggerBuilder::new()
        .with_debug_logger_options(DebugLoggerOptions::new().with_max_level(LevelFilter::Debug))
        .with_log_level(LevelFilter::Info)
        .build()
        .init()
        .unwrap();

    assert_eq!(log::max_level(), LevelFilter::Info);
    assert!(log::logger().enabled(&Metadata::builder().level(Level::Info).build()));
    assert!(!log::logger().enabled(&Metadata::builder().level(Level::Debug).build()));
}
