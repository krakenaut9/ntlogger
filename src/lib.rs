#![no_std]

mod loggers;
mod time_util;

pub use crate::loggers::{
    Entry, Sink,
    dbgprint::{DbgPrintComponent, DbgPrintLevel},
    debug::DebugLoggerOptions,
};
use crate::{irql::KeGetCurrentIrql, loggers::debug::DebuggerLogger, time_util::current_time};
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, Ordering},
};
use log::{Level, LevelFilter, Log, Metadata, Record, SetLoggerError};
use windows_sys::Wdk::System::SystemServices::{PsGetCurrentProcessId, PsGetCurrentThreadId};

// wdm.h exports KeGetCurrentIrql from ntoskrnl.exe on x64 and from hal.dll on every
// other architecture; windows-sys names ntoskrnl.exe for all of them. `link!` declares the
// function `pub`, so a private module keeps it out of the crate's API.
mod irql {
    #[cfg(target_arch = "x86_64")]
    windows_link::link!("ntoskrnl.exe" "system" fn KeGetCurrentIrql() -> u8);
    #[cfg(not(target_arch = "x86_64"))]
    windows_link::link!("hal.dll" "system" fn KeGetCurrentIrql() -> u8);
}

// The logger that `log` calls. It stays empty until `NtLogger::set_logger` moves a
// logger in, which only the call that `log::set_logger` accepts does: `log` accepts one
// logger only, so a second call changes nothing.
static LOGGER: LoggerCell = LoggerCell::new();

pub struct NtLoggerBuilder {
    log_level: LevelFilter,
    debug_logger: Option<DebugLoggerOptions>,
    sinks: &'static [&'static dyn Sink],
}

impl NtLoggerBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The logger's level: the most verbose records it logs, [`LevelFilter::Info`] unless
    /// set. A sink takes the records within it, or fewer with a level of its own
    /// ([`Sink::max_level`], [`DebugLoggerOptions::with_max_level`]), never more.
    ///
    /// [`NtLogger::set_logger`] gives this level to `log` with [`log::set_max_level`],
    /// and from then on [`log::max_level`] is the logger's level. The `log` macros check it
    /// before they build a record, and the logger checks it again, for a record passed to
    /// `log::logger().log` directly. So `log::set_max_level` changes it while the driver
    /// runs, from an IOCTL handler for example. It is a single atomic store, so any IRQL
    /// will do:
    ///
    /// ```
    /// log::set_max_level(log::LevelFilter::Debug);
    /// ```
    ///
    /// `log` can also remove levels at compile time, at no cost at run time. With this in
    /// the driver's `Cargo.toml`, release builds compile `debug!` and `trace!` to nothing:
    ///
    /// ```toml
    /// log = { version = "0.4", features = ["release_max_level_info"] }
    /// ```
    ///
    /// This is `log`'s level; the level the debugger filters records by is
    /// [`DebugLoggerOptions::with_dbgprint_level`].
    #[must_use]
    pub fn with_log_level(mut self, level: LevelFilter) -> Self {
        self.log_level = level;
        self
    }

    /// Writes records to the kernel debugger, each as one line: the [`Entry`]'s text, then a
    /// newline. A line takes at most 511 bytes, which `DbgPrintEx` prints whole; a longer one
    /// is cut at a character boundary and ends with `...`. A record logged above DIRQL, where
    /// `DbgPrintEx` can deadlock, is counted instead of printed:
    /// [`NtLogger::debug_logger_skipped_above_dirql`] returns how many.
    ///
    /// Unless options were given with
    /// [`with_debug_logger_options`](Self::with_debug_logger_options), it uses the defaults:
    /// every record the logger's level lets through, [`DbgPrintComponent::IHV_DRIVER`] and
    /// [`DbgPrintLevel::ERROR`], which a kernel debugger always shows.
    #[must_use]
    pub fn with_debug_logger(mut self) -> Self {
        self.debug_logger
            .get_or_insert_with(DebugLoggerOptions::new);
        self
    }

    /// Writes records to the kernel debugger as
    /// [`with_debug_logger`](Self::with_debug_logger) does, with `options`.
    ///
    /// ```
    /// use ntlogger::{DbgPrintComponent, DbgPrintLevel, DebugLoggerOptions, NtLoggerBuilder};
    ///
    /// // A network driver whose records are shown once the debugger enables INFO for its
    /// // component: `ed nt!Kd_IHVNETWORK_Mask 0x8`.
    /// let logger = NtLoggerBuilder::new()
    ///     .with_debug_logger_options(
    ///         DebugLoggerOptions::new()
    ///             .with_component(DbgPrintComponent::IHV_NETWORK)
    ///             .with_dbgprint_level(DbgPrintLevel::INFO),
    ///     )
    ///     .build();
    /// // ...then `NtLogger::set_logger(logger)`.
    /// ```
    #[must_use]
    pub fn with_debug_logger_options(mut self, options: DebugLoggerOptions) -> Self {
        self.debug_logger = Some(options);
        self
    }

    /// Writes records to `sinks` as well: sinks of the driver's own, written to after the
    /// built-in ones, in the order given. Each takes the records the logger's level lets
    /// through, up to its own [`Sink::max_level`].
    ///
    /// The list can be as long as needed, and nothing is allocated: the logger keeps the
    /// `'static` list itself, usually a `static` array, as in [`Sink`]'s example. A second
    /// call replaces the list.
    #[must_use]
    pub fn with_sinks(mut self, sinks: &'static [&'static dyn Sink]) -> Self {
        self.sinks = sinks;
        self
    }

    #[must_use]
    pub fn build(self) -> NtLogger {
        let Self {
            log_level,
            debug_logger,
            sinks,
        } = self;
        NtLogger {
            log_level,
            debugger: debug_logger.map(DebuggerLogger::new),
            sinks,
        }
    }
}

impl Default for NtLoggerBuilder {
    fn default() -> Self {
        Self {
            log_level: LevelFilter::Info,
            debug_logger: None,
            sinks: &[],
        }
    }
}

pub struct NtLogger {
    /// The level `set_logger` gives `log`, which is the logger's level from then on.
    log_level: LevelFilter,
    /// The kernel debugger sink, if it is enabled.
    debugger: Option<DebuggerLogger>,
    /// The driver's own sinks.
    sinks: &'static [&'static dyn Sink],
}

impl NtLogger {
    /// Installs `logger` as the logger the `log` macros write to, and sets `log`'s level to
    /// the logger's ([`NtLoggerBuilder::with_log_level`]), replacing any level set
    /// before. From then on, `log::set_max_level` changes the logger's level.
    ///
    /// Only the first call installs a logger. A later one fails and changes nothing, so
    /// the logger in use and `log`'s level stay as they are.
    ///
    /// # Errors
    ///
    /// If a logger is already installed, by an earlier call or by `log::set_logger`.
    pub fn set_logger(logger: Self) -> Result<(), SetLoggerError> {
        let log_level = logger.log_level;

        LOGGER.install(logger)?;

        log::set_max_level(log_level);

        Ok(())
    }

    /// How many records the kernel debugger sink has not printed because they were logged
    /// above DIRQL, where `DbgPrintEx` can deadlock: 0 before [`set_logger`](Self::set_logger),
    /// and if the logger in use has no debugger sink.
    #[must_use]
    pub fn debug_logger_skipped_above_dirql() -> usize {
        LOGGER
            .get()
            .and_then(|logger| logger.debugger.as_ref())
            .map_or(0, DebuggerLogger::skipped_above_dirql)
    }

    /// Every sink: the built-in ones that are enabled, then the driver's own.
    fn sinks(&self) -> impl Iterator<Item = &dyn Sink> {
        // Exhaustive, so that a built-in sink added as a field cannot be left out here.
        let Self {
            log_level: _,
            debugger,
            sinks,
        } = self;
        let built_in = debugger.iter().map(|debugger| debugger as &dyn Sink);
        built_in.chain(sinks.iter().copied())
    }

    /// The sinks that take a record at `level` while `log`'s level, which is the logger's,
    /// is `log_level`: none above it, and otherwise those whose own level takes it.
    fn sinks_taking(
        &self,
        level: Level,
        log_level: LevelFilter,
    ) -> impl Iterator<Item = &dyn Sink> {
        let within_log_level = level <= log_level;

        self.sinks()
            .filter(move |&sink| within_log_level && level <= sink.max_level())
    }
}

// A logger built with the defaults: it has no sink, so it logs nothing.
impl Default for NtLogger {
    fn default() -> Self {
        NtLoggerBuilder::new().build()
    }
}

// Both methods check `log`'s level, which is the logger's, although the `log` macros check
// it before calling them: code can also call `log::logger()` directly, skipping the macros,
// and `log` leaves the filtering to the logger. For a record from the macros, the check
// passes, and only rereads a value the macro has just read.
impl Log for NtLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.sinks_taking(metadata.level(), log::max_level())
            .next()
            .is_some()
    }

    fn log(&self, record: &Record) {
        let mut sinks = self
            .sinks_taking(record.level(), log::max_level())
            .peekable();

        // The IDs, the IRQL and the time are read only for a record that some sink takes.
        if sinks.peek().is_none() {
            return;
        }
        // SAFETY: none of these functions takes an argument, and each may be called at any
        // IRQL.
        let (process_id, thread_id, irql) = unsafe {
            (
                PsGetCurrentProcessId(),
                PsGetCurrentThreadId(),
                KeGetCurrentIrql(),
            )
        };
        let entry = Entry::new(
            record,
            process_id.addr(),
            thread_id.addr(),
            irql,
            current_time(),
        );
        for sink in sinks {
            sink.write(&entry);
        }
    }

    fn flush(&self) {
        for sink in self.sinks() {
            sink.flush();
        }
    }
}

/// Where the logger that `log` calls lives: [`LOGGER`].
struct LoggerCell {
    /// Set, with Release, once `logger` holds the installed logger; never cleared.
    ready: AtomicBool,
    /// Written once, by `install`, before `ready` is set.
    logger: UnsafeCell<Option<NtLogger>>,
}

// SAFETY: `logger` is written only by `install`, once `log::set_logger` has accepted this
// cell, which it does for one logger only, and before `ready` is set; `get` reads it only
// after seeing `ready` set, with Acquire. So that write happens before every read, and no
// read overlaps it. After that, processors share the logger by reference, which the bound
// keeps safe.
unsafe impl Sync for LoggerCell where NtLogger: Sync {}

impl LoggerCell {
    const fn new() -> Self {
        Self {
            ready: AtomicBool::new(false),
            logger: UnsafeCell::new(None),
        }
    }

    /// Makes this cell `log`'s logger, then moves `logger` in. Fails, changing nothing, if
    /// `log` already has a logger: this cell or any other.
    fn install(&'static self, logger: NtLogger) -> Result<(), SetLoggerError> {
        log::set_logger(self)?;
        // SAFETY: `log::set_logger` accepts one logger only, so no other call gets here,
        // and `ready` is still false, so nothing reads `logger` yet.
        unsafe {
            *self.logger.get() = Some(logger);
        }
        // Release, after the logger is written: a record that sees `ready` sees the logger.
        self.ready.store(true, Ordering::Release);
        Ok(())
    }

    /// The installed logger, or `None` before `set_logger`.
    fn get(&self) -> Option<&NtLogger> {
        if self.ready.load(Ordering::Acquire) {
            // SAFETY: `ready` is set after the only write to `logger`, and this Acquire load
            // pairs with that Release store, so the write is complete and visible here.
            unsafe { (*self.logger.get()).as_ref() }
        } else {
            None
        }
    }
}

impl Log for LoggerCell {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.get().is_some_and(|logger| logger.enabled(metadata))
    }

    fn log(&self, record: &Record) {
        if let Some(logger) = self.get() {
            logger.log(record);
        }
    }

    fn flush(&self) {
        if let Some(logger) = self.get() {
            logger.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::{ptr, sync::atomic::AtomicUsize};

    /// A sink of the driver's own that drops what it gets, with a level of its own.
    struct Quiet(LevelFilter);

    impl Sink for Quiet {
        fn max_level(&self) -> LevelFilter {
            self.0
        }

        fn write(&self, _entry: &Entry<'_>) {}
    }

    /// A sink of the driver's own that counts the flushes it gets.
    struct Flushes(AtomicUsize);

    impl Sink for Flushes {
        fn write(&self, _entry: &Entry<'_>) {}

        fn flush(&self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The addresses of `sinks`, which tell them apart.
    fn addresses<'a>(sinks: impl IntoIterator<Item = &'a dyn Sink>) -> heapless::Vec<*const (), 4> {
        sinks
            .into_iter()
            .map(|sink| ptr::from_ref(sink).cast::<()>())
            .collect()
    }

    #[test]
    fn the_level_is_info_unless_with_log_level_sets_another() {
        let default = NtLoggerBuilder::new().build();
        let off = NtLoggerBuilder::new()
            .with_log_level(LevelFilter::Off)
            .build();

        assert_eq!(default.log_level, LevelFilter::Info);
        assert_eq!(NtLogger::default().log_level, LevelFilter::Info);
        assert_eq!(off.log_level, LevelFilter::Off);
    }

    #[test]
    fn with_debug_logger_uses_the_default_options() {
        let logger = NtLoggerBuilder::new().with_debug_logger().build();

        assert_eq!(
            logger.debugger.as_ref().map(DebuggerLogger::options),
            Some(DebugLoggerOptions::new())
        );
    }

    #[test]
    fn with_debug_logger_options_uses_the_options() {
        let options = DebugLoggerOptions::new()
            .with_max_level(LevelFilter::Debug)
            .with_component(DbgPrintComponent::IHV_NETWORK)
            .with_dbgprint_level(DbgPrintLevel::INFO);
        let logger = NtLoggerBuilder::new()
            .with_debug_logger_options(options)
            .build();

        assert_eq!(
            logger.debugger.as_ref().map(DebuggerLogger::options),
            Some(options)
        );
    }

    #[test]
    fn with_debug_logger_keeps_the_options_given_before() {
        let options = DebugLoggerOptions::new().with_dbgprint_level(DbgPrintLevel::WARNING);
        let logger = NtLoggerBuilder::new()
            .with_debug_logger_options(options)
            .with_debug_logger()
            .build();
        assert_eq!(
            logger.debugger.as_ref().map(DebuggerLogger::options),
            Some(options)
        );
    }

    #[test]
    fn a_logger_without_sinks_logs_nothing() {
        let logger = NtLogger::default();

        assert!(
            logger
                .sinks_taking(Level::Error, LevelFilter::Trace)
                .next()
                .is_none()
        );
    }

    #[test]
    fn each_sink_takes_the_records_up_to_its_own_level() {
        static UP_TO_DEBUG: Quiet = Quiet(LevelFilter::Debug);
        static EVERY: Quiet = Quiet(LevelFilter::Trace);
        static NOTHING: Quiet = Quiet(LevelFilter::Off);
        static SINKS: [&dyn Sink; 3] = [&UP_TO_DEBUG, &EVERY, &NOTHING];

        let logger = NtLoggerBuilder::new()
            .with_debug_logger_options(DebugLoggerOptions::new().with_max_level(LevelFilter::Warn))
            .with_sinks(&SINKS)
            .build();
        let debugger: &dyn Sink = logger.debugger.as_ref().unwrap();

        // The debugger first, then the driver's own sinks, in their order.
        let taking_warn: [&dyn Sink; 3] = [debugger, &UP_TO_DEBUG, &EVERY];
        let taking_debug: [&dyn Sink; 2] = [&UP_TO_DEBUG, &EVERY];
        let taking_trace: [&dyn Sink; 1] = [&EVERY];

        // With `log`'s level at Trace, only their own levels count.
        assert_eq!(
            addresses(logger.sinks_taking(Level::Warn, LevelFilter::Trace)),
            addresses(taking_warn)
        );
        assert_eq!(
            addresses(logger.sinks_taking(Level::Debug, LevelFilter::Trace)),
            addresses(taking_debug)
        );
        assert_eq!(
            addresses(logger.sinks_taking(Level::Trace, LevelFilter::Trace)),
            addresses(taking_trace)
        );
    }

    #[test]
    fn no_sink_takes_a_record_above_the_loggers_level() {
        static EVERY: Quiet = Quiet(LevelFilter::Trace);
        static SINKS: [&dyn Sink; 1] = [&EVERY];

        let logger = NtLoggerBuilder::new()
            .with_debug_logger()
            .with_sinks(&SINKS)
            .build();
        let debugger: &dyn Sink = logger.debugger.as_ref().unwrap();
        let both: [&dyn Sink; 2] = [debugger, &EVERY];

        // Their own levels take every record, so only `log`'s level stops one.
        assert_eq!(
            addresses(logger.sinks_taking(Level::Info, LevelFilter::Info)),
            addresses(both)
        );
        assert!(
            logger
                .sinks_taking(Level::Debug, LevelFilter::Info)
                .next()
                .is_none()
        );
        assert!(
            logger
                .sinks_taking(Level::Error, LevelFilter::Off)
                .next()
                .is_none()
        );
    }

    #[test]
    fn with_sinks_replaces_the_sinks_given_before() {
        static FIRST: Quiet = Quiet(LevelFilter::Error);
        static SECOND: Quiet = Quiet(LevelFilter::Warn);
        static FIRST_SINKS: [&dyn Sink; 1] = [&FIRST];
        static SECOND_SINKS: [&dyn Sink; 1] = [&SECOND];

        let logger = NtLoggerBuilder::new()
            .with_sinks(&FIRST_SINKS)
            .with_sinks(&SECOND_SINKS)
            .build();
        assert_eq!(addresses(logger.sinks()), addresses(SECOND_SINKS));
    }

    #[test]
    fn flush_reaches_every_sink() {
        static FIRST: Flushes = Flushes(AtomicUsize::new(0));
        static SECOND: Flushes = Flushes(AtomicUsize::new(0));
        static SINKS: [&dyn Sink; 2] = [&FIRST, &SECOND];

        let logger = NtLoggerBuilder::new()
            .with_debug_logger()
            .with_sinks(&SINKS)
            .build();
        logger.flush();

        let flushes = [&FIRST, &SECOND].map(|sink| sink.0.load(Ordering::Relaxed));
        assert_eq!(flushes, [1, 1]);
    }
}
