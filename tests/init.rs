//! `NtLogger::init` installs the logger once and refuses any later call.
//!
//! A file of its own, so that it runs in a process of its own: `log` takes one logger
//! per process. It never logs, since that would run kernel functions.

use log::{Level, LevelFilter, Metadata};
use ntlogger::NtLoggerBuilder;

#[test]
fn a_second_init_fails_and_the_installed_logger_stays() {
    let info = Metadata::builder().level(Level::Info).build();

    NtLoggerBuilder::new()
        .with_debug_logger()
        .build()
        .init()
        .unwrap();
    assert_eq!(log::max_level(), LevelFilter::Info);
    assert!(log::logger().enabled(&info));

    // Installed, this logger would change `log`'s level and take no record. Having no sink,
    // it reports its failure to none, so the report reads nothing from the kernel.
    assert!(
        NtLoggerBuilder::new()
            .with_log_level(LevelFilter::Trace)
            .build()
            .init()
            .is_err()
    );
    assert_eq!(log::max_level(), LevelFilter::Info);
    assert!(log::logger().enabled(&info));
}
