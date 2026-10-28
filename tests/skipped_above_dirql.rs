//! `KernelLogger::debug_logger_skipped_above_dirql` reads the count of the debugger sink in
//! use, through the installed logger.
//!
//! A file of its own, so that it runs in a process of its own: `log` takes one logger
//! per process. It never logs, since that would run kernel functions.

use ntlogger::{KernelLogger, KernelLoggerBuilder};

#[test]
fn nothing_is_counted_before_a_record_is_skipped() {
    // No logger installed yet.
    assert_eq!(KernelLogger::debug_logger_skipped_above_dirql(), 0);

    KernelLogger::set_logger(KernelLoggerBuilder::new().with_debug_logger().build()).unwrap();
    assert_eq!(KernelLogger::debug_logger_skipped_above_dirql(), 0);
}
