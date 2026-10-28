//! `log::set_max_level` changes the installed logger's level while it runs, up as well as
//! down.
//!
//! A file of its own, so that it runs in a process of its own: `log` takes one logger
//! per process. It never logs, since that would run kernel functions.

use log::{Level, LevelFilter, Metadata};
use ntlogger::{KernelLogger, KernelLoggerBuilder};

/// The metadata of a record at `level`.
fn at(level: Level) -> Metadata<'static> {
    Metadata::builder().level(level).build()
}

#[test]
fn log_set_max_level_changes_the_loggers_level() {
    KernelLogger::set_logger(KernelLoggerBuilder::new().with_debug_logger().build()).unwrap();

    // The default level.
    assert_eq!(log::max_level(), LevelFilter::Info);
    assert!(!log::logger().enabled(&at(Level::Debug)));

    log::set_max_level(LevelFilter::Debug);

    assert!(log::logger().enabled(&at(Level::Debug)));

    log::set_max_level(LevelFilter::Off);

    assert!(!log::logger().enabled(&at(Level::Error)));
}
