//! `NtLogger::set_logger` installs the logger once and refuses any later call.
//!
//! A file of its own, so that it runs in a process of its own: `log` takes one logger
//! per process. It never logs, since that would run kernel functions.

use log::{Level, LevelFilter, Metadata};
use ntlogger::{NtLogger, NtLoggerBuilder};

fn logger_at(level: LevelFilter) -> NtLogger {
    NtLoggerBuilder::new()
        .with_debug_logger()
        .with_log_level(level)
        .build()
}

#[test]
fn a_second_set_logger_fails_and_changes_nothing() {
    let debug = Metadata::builder().level(Level::Debug).build();

    NtLogger::set_logger(logger_at(LevelFilter::Info)).unwrap();
    assert_eq!(log::max_level(), LevelFilter::Info);
    assert!(!log::logger().enabled(&debug));

    assert!(NtLogger::set_logger(logger_at(LevelFilter::Trace)).is_err());
    assert_eq!(log::max_level(), LevelFilter::Info);
    assert!(!log::logger().enabled(&debug));
}
