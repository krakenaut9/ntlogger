//! A logger built without any sink, not even the debugger, logs nothing, whatever its
//! level.
//!
//! A file of its own, so that it runs in a process of its own: `log` takes one logger
//! per process. It never logs, since that would run kernel functions.

use log::{Level, LevelFilter, Metadata};
use ntlogger::{NtLogger, NtLoggerBuilder};

#[test]
fn a_logger_without_sinks_logs_nothing() {
    NtLogger::set_logger(
        NtLoggerBuilder::new()
            .with_log_level(LevelFilter::Trace)
            .build(),
    )
    .unwrap();

    assert_eq!(log::max_level(), LevelFilter::Trace);
    assert!(!log::logger().enabled(&Metadata::builder().level(Level::Error).build()));
}
