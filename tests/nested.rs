//! A logger that a driver nests in a logger of its own, rather than installing it, takes the
//! records within `log`'s level, which the driver sets: the logger's own level is only what
//! `NtLogger::init` gives `log`.
//!
//! A file of its own, so that it runs in a process of its own: `log` has one level per
//! process. It never logs, since that would run kernel functions.

use log::{Level, LevelFilter, Log, Metadata};
use ntlogger::NtLoggerBuilder;

/// The metadata of a record at `level`.
fn at(level: Level) -> Metadata<'static> {
    Metadata::builder().level(level).build()
}

#[test]
fn a_nested_logger_takes_the_records_within_logs_level() {
    let logger = NtLoggerBuilder::new()
        .with_debug_logger()
        .with_log_level(LevelFilter::Trace)
        .build();

    // `log`'s level is Off until the driver sets it, whatever the logger's own.
    assert!(!logger.enabled(&at(Level::Error)));

    log::set_max_level(LevelFilter::Info);

    assert!(logger.enabled(&at(Level::Info)));
    assert!(!logger.enabled(&at(Level::Debug)));
}
