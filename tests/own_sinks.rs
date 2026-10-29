//! A driver's own sinks are enough for a logger, without the debugger, and each takes the
//! records up to its own level.
//!
//! A file of its own, so that it runs in a process of its own: `log` takes one logger
//! per process. It never logs, since that would run kernel functions.

use log::{Level, LevelFilter, Metadata};
use ntlogger::{Entry, NtLoggerBuilder, Sink};

/// A sink that drops what it gets, with a level of its own.
struct Quiet(LevelFilter);

impl Sink for Quiet {
    fn max_level(&self) -> LevelFilter {
        self.0
    }

    fn write(&self, _entry: &Entry<'_>) {}
}

static UP_TO_WARN: Quiet = Quiet(LevelFilter::Warn);
static SINKS: [&dyn Sink; 1] = [&UP_TO_WARN];

#[test]
fn the_drivers_own_sinks_take_the_records_up_to_their_level() {
    NtLoggerBuilder::new()
        .with_sinks(&SINKS)
        .build()
        .init()
        .unwrap();

    // The logger's level, Info by default, lets an info record through; the sink's own
    // level stops it.
    assert_eq!(log::max_level(), LevelFilter::Info);
    assert!(log::logger().enabled(&Metadata::builder().level(Level::Warn).build()));
    assert!(!log::logger().enabled(&Metadata::builder().level(Level::Info).build()));
}
