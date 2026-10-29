//! A logger that `NtLogger::init` could not install reports the failure to its own sinks,
//! as an error record within its own level: `log`'s level is the installed logger's.
//!
//! A file of its own, so that it runs in a process of its own: `log` takes one logger
//! per process. Its sink takes no record, so the logger reads nothing from the kernel for
//! it; the sink only counts how often the logger asks for its level, which the logger does
//! for every record within its own level.

use core::sync::atomic::{AtomicUsize, Ordering};
use log::LevelFilter;
use ntlogger::{Entry, NtLoggerBuilder, Sink};

/// Counts how often its level is asked for, and takes no record.
struct Asked(AtomicUsize);

impl Sink for Asked {
    fn max_level(&self) -> LevelFilter {
        self.0.fetch_add(1, Ordering::Relaxed);

        LevelFilter::Off
    }

    fn write(&self, _entry: &Entry<'_>) {}
}

static ASKED: Asked = Asked(AtomicUsize::new(0));
static SINKS: [&dyn Sink; 1] = [&ASKED];

#[test]
fn a_failed_init_reports_an_error_to_its_own_sinks() {
    // `log`'s level, the installed logger's, takes no record: a report filtered by it would
    // reach no sink.
    NtLoggerBuilder::new()
        .with_log_level(LevelFilter::Off)
        .build()
        .init()
        .unwrap();

    // Only an error record is within this logger's level.
    let failed = NtLoggerBuilder::new()
        .with_sinks(&SINKS)
        .with_log_level(LevelFilter::Error)
        .build()
        .init();

    assert!(failed.is_err());
    assert_eq!(ASKED.0.load(Ordering::Relaxed), 1);
}
