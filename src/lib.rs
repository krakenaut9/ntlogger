#![no_std]
#![doc = include_str!("../README.md")]

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
    fmt,
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

// The logger that `log` calls. It stays empty until `NtLogger::init` moves a logger in,
// which only the call that `log::set_logger` accepts does: `log` accepts one logger only,
// so a later call leaves it as it is.
static LOGGER: LoggerCell = LoggerCell::new();

/// Sets up an [`NtLogger`]: the level it takes records up to, and the sinks it writes them
/// to, the kernel debugger and sinks of the driver's own. [`build`](Self::build) makes the
/// logger, and [`NtLogger::init`] installs it.
///
/// ```no_run
/// use log::LevelFilter;
/// use ntlogger::NtLoggerBuilder;
///
/// fn setup_logger() -> Result<(), log::SetLoggerError> {
///     NtLoggerBuilder::new()
///         .with_debug_logger()
///         .with_log_level(LevelFilter::Info)
///         .build()
///         .init()
/// }
/// ```
#[derive(Clone)]
pub struct NtLoggerBuilder {
    log_level: LevelFilter,
    debug_logger: Option<DebugLoggerOptions>,
    sinks: &'static [&'static dyn Sink],
}

impl NtLoggerBuilder {
    /// A builder with the defaults: the level [`LevelFilter::Info`], and no sink. A logger
    /// built without one logs nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The logger's level: the most verbose records it logs, [`LevelFilter::Info`] unless
    /// set. A sink takes the records within it, or fewer with a level of its own
    /// ([`Sink::max_level`], [`DebugLoggerOptions::with_max_level`]), never more.
    ///
    /// [`NtLogger::init`] gives this level to `log` with [`log::set_max_level`],
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
    /// is cut at a character boundary and ends with `...`. Text that holds a NUL is printed
    /// only up to the NUL, then the newline. The text goes to the debugger as UTF-8 bytes, so a
    /// debugger that reads them in a code page such as Windows-1252 shows `é` as `Ã©`. A record
    /// logged above DIRQL, where `DbgPrintEx` can deadlock, is counted instead of printed:
    /// [`NtLogger::debug_logger_skipped_above_dirql`] returns how many.
    ///
    /// Unless options were given with
    /// [`with_debug_logger_options`](Self::with_debug_logger_options), it uses the defaults:
    /// every record the logger's level lets through, [`DbgPrintComponent::IHV_DRIVER`] and
    /// [`DbgPrintLevel::ERROR`], which a kernel debugger always shows.
    ///
    /// ```no_run
    /// use ntlogger::NtLoggerBuilder;
    ///
    /// // Every record from INFO up, shown whenever a kernel debugger is attached.
    /// fn setup_logger() -> Result<(), log::SetLoggerError> {
    ///     NtLoggerBuilder::new().with_debug_logger().build().init()
    /// }
    /// ```
    #[must_use]
    pub fn with_debug_logger(mut self) -> Self {
        self.debug_logger
            .get_or_insert_with(DebugLoggerOptions::new);
        self
    }

    /// Writes records to the kernel debugger as
    /// [`with_debug_logger`](Self::with_debug_logger) does, with `options`.
    ///
    /// ```no_run
    /// use ntlogger::{DbgPrintComponent, DbgPrintLevel, DebugLoggerOptions, NtLoggerBuilder};
    ///
    /// // A network driver whose records are shown once the debugger enables INFO for its
    /// // component: `ed nt!Kd_IHVNETWORK_Mask 0x8`.
    /// NtLoggerBuilder::new()
    ///     .with_debug_logger_options(
    ///         DebugLoggerOptions::new()
    ///             .with_component(DbgPrintComponent::IHV_NETWORK)
    ///             .with_dbgprint_level(DbgPrintLevel::INFO),
    ///     )
    ///     .build()
    ///     .init()?;
    /// # Ok::<(), log::SetLoggerError>(())
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
    /// `'static` list itself, usually a `static` array. A second call replaces the list.
    ///
    /// ```no_run
    /// use ntlogger::{Entry, NtLoggerBuilder, Sink};
    ///
    /// /// A sink that keeps nothing.
    /// struct Discard;
    ///
    /// impl Sink for Discard {
    ///     fn write(&self, _entry: &Entry<'_>) {}
    /// }
    ///
    /// static FIRST: Discard = Discard;
    /// static SECOND: Discard = Discard;
    ///
    /// // Written to in this order, after the kernel debugger.
    /// static SINKS: [&dyn Sink; 2] = [&FIRST, &SECOND];
    ///
    /// fn setup_logger() -> Result<(), log::SetLoggerError> {
    ///     NtLoggerBuilder::new()
    ///         .with_debug_logger()
    ///         .with_sinks(&SINKS)
    ///         .build()
    ///         .init()
    /// }
    /// ```
    #[must_use]
    pub fn with_sinks(mut self, sinks: &'static [&'static dyn Sink]) -> Self {
        self.sinks = sinks;
        self
    }

    /// Builds the logger. [`NtLogger::init`] installs it as the logger the `log` macros write
    /// to; a driver can also nest it, as a [`Log`], in a logger of its own:
    ///
    /// ```no_run
    /// use log::{Log, Metadata, Record};
    /// use ntlogger::{NtLogger, NtLoggerBuilder};
    ///
    /// /// A driver's own logger, which hands each record on to ntlogger's.
    /// struct DriverLogger {
    ///     nested: NtLogger,
    /// }
    ///
    /// impl Log for DriverLogger {
    ///     fn enabled(&self, metadata: &Metadata) -> bool {
    ///         self.nested.enabled(metadata)
    ///     }
    ///
    ///     fn log(&self, record: &Record) {
    ///         self.nested.log(record);
    ///     }
    ///
    ///     fn flush(&self) {
    ///         self.nested.flush();
    ///     }
    /// }
    ///
    /// let logger = DriverLogger {
    ///     nested: NtLoggerBuilder::new().with_debug_logger().build(),
    /// };
    /// ```
    #[must_use = "a built logger does nothing until `init` installs it"]
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

impl fmt::Debug for NtLoggerBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Exhaustive, so that a setting added as a field cannot be left out here.
        let Self {
            log_level,
            debug_logger,
            sinks,
        } = self;

        f.debug_struct("NtLoggerBuilder")
            .field("log_level", log_level)
            .field("debug_logger", debug_logger)
            .field("sinks", &SinksDebug(sinks))
            .finish()
    }
}

/// The logger that [`NtLoggerBuilder::build`] makes. [`init`](Self::init) installs it as the
/// logger the `log` macros write to; a driver can also nest it, as a [`Log`], in a logger of
/// its own. A nested logger takes the records within `log`'s level, which the driver then
/// sets itself with `log::set_max_level`: only `init` gives `log` the level set with
/// [`with_log_level`](NtLoggerBuilder::with_log_level).
pub struct NtLogger {
    /// The level `init` gives `log`, which is the logger's level from then on. If `init`
    /// fails, it filters the report of the failure instead: `log`'s level then belongs to
    /// the logger installed before.
    log_level: LevelFilter,
    /// The kernel debugger sink, if it is enabled.
    debugger: Option<DebuggerLogger>,
    /// The driver's own sinks.
    sinks: &'static [&'static dyn Sink],
}

impl NtLogger {
    /// Installs this logger as the logger the `log` macros write to, and sets `log`'s level
    /// to the logger's ([`NtLoggerBuilder::with_log_level`]), replacing any level set before.
    /// From then on, `log::set_max_level` changes the logger's level. It works at any IRQL,
    /// and a driver calls it once, in `DriverEntry`:
    ///
    /// ```no_run
    /// use log::LevelFilter;
    /// use ntlogger::NtLoggerBuilder;
    ///
    /// NtLoggerBuilder::new()
    ///     .with_debug_logger()
    ///     .with_log_level(LevelFilter::Info)
    ///     .build()
    ///     .init()?;
    /// # Ok::<(), log::SetLoggerError>(())
    /// ```
    ///
    /// Only the first call installs a logger. A later one fails, and the logger in use and
    /// `log`'s level stay as they are. The logger it was called on is dropped, but first
    /// reports the failure to its own sinks: an error record with the target `ntlogger`,
    /// which each takes if its level and the logger's let errors through.
    ///
    /// # Errors
    ///
    /// If a logger is already installed, by an earlier call or by `log::set_logger`.
    pub fn init(self) -> Result<(), SetLoggerError> {
        let log_level = self.log_level;

        if let Err((error, logger)) = LOGGER.install(self) {
            logger.report_not_installed();
            return Err(error);
        }

        log::set_max_level(log_level);

        Ok(())
    }

    /// How many records the kernel debugger sink has not printed because they were logged
    /// above DIRQL, where `DbgPrintEx` can deadlock: 0 before [`init`](Self::init) installs a
    /// logger, and if that logger has no debugger sink.
    ///
    /// It is an atomic read, so any IRQL will do, for example when the driver unloads:
    ///
    /// ```no_run
    /// use ntlogger::NtLogger;
    ///
    /// let skipped = NtLogger::debug_logger_skipped_above_dirql();
    /// if skipped > 0 {
    ///     log::warn!("{skipped} records were logged above DIRQL and not printed");
    /// }
    /// ```
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

    /// The sinks that take a record at `level` while the logger's level is `log_level`: none
    /// above it, and otherwise those whose own level takes it.
    fn sinks_taking(
        &self,
        level: Level,
        log_level: LevelFilter,
    ) -> impl Iterator<Item = &dyn Sink> {
        let within_log_level = level <= log_level;

        self.sinks()
            .filter(move |&sink| within_log_level && level <= sink.max_level())
    }

    /// Writes `record` to the sinks that take it while the logger's level is `log_level`.
    fn write_record(&self, record: &Record<'_>, log_level: LevelFilter) {
        let mut sinks = self.sinks_taking(record.level(), log_level).peekable();

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

    /// Tells the sinks that `init` could not install this logger. The logger's own level
    /// filters the report, since `log`'s belongs to the logger installed before.
    fn report_not_installed(&self) {
        self.write_record(
            &Record::builder()
                .args(format_args!(
                    "a logger is already installed, so this one is not used"
                ))
                .level(Level::Error)
                .target("ntlogger")
                .build(),
            self.log_level,
        );
    }
}

// A logger built with the defaults: it has no sink, so it logs nothing.
impl Default for NtLogger {
    fn default() -> Self {
        NtLoggerBuilder::new().build()
    }
}

impl fmt::Debug for NtLogger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Exhaustive, so that a sink or setting added as a field cannot be left out here.
        let Self {
            log_level,
            debugger,
            sinks,
        } = self;

        f.debug_struct("NtLogger")
            .field("log_level", log_level)
            .field("debugger", debugger)
            .field("sinks", &SinksDebug(sinks))
            .finish()
    }
}

/// The driver's own sinks as `Debug` shows them: one `<dyn Sink>` for each, since a sink need
/// not implement `Debug`.
struct SinksDebug<'a>(&'a [&'a dyn Sink]);

impl fmt::Debug for SinksDebug<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut list = f.debug_list();
        for _ in self.0 {
            list.entry(&format_args!("<dyn Sink>"));
        }

        list.finish()
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
        self.write_record(record, log::max_level());
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

    /// Makes this cell `log`'s logger, then moves `logger` in. If `log` already has a logger,
    /// this cell or any other, it changes nothing and hands `logger` back with the error.
    fn install(&'static self, logger: NtLogger) -> Result<(), (SetLoggerError, NtLogger)> {
        if let Err(error) = log::set_logger(self) {
            return Err((error, logger));
        }

        // SAFETY: `log::set_logger` accepts one logger only, so no other call gets here,
        // and `ready` is still false, so nothing reads `logger` yet.
        unsafe {
            *self.logger.get() = Some(logger);
        }
        // Release, after the logger is written: a record that sees `ready` sees the logger.
        self.ready.store(true, Ordering::Release);

        Ok(())
    }

    /// The installed logger, or `None` before `init`.
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
    use core::{fmt::Write, hash::Hash, ptr, sync::atomic::AtomicUsize};

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

    /// `value` as `{:?}` shows it.
    fn debug_of(value: &impl fmt::Debug) -> heapless::String<512> {
        let mut shown = heapless::String::new();
        write!(shown, "{value:?}").unwrap();

        shown
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

    #[test]
    fn a_builder_shows_its_settings_and_a_placeholder_per_sink() {
        static EVERY: Quiet = Quiet(LevelFilter::Trace);
        static SINKS: [&dyn Sink; 2] = [&EVERY, &EVERY];

        let builder = NtLoggerBuilder::new()
            .with_log_level(LevelFilter::Warn)
            .with_debug_logger()
            .with_sinks(&SINKS);

        let mut expected = heapless::String::<512>::new();
        write!(
            expected,
            "NtLoggerBuilder {{ log_level: Warn, debug_logger: Some({:?}), sinks: [<dyn Sink>, <dyn Sink>] }}",
            DebugLoggerOptions::new()
        )
        .unwrap();

        assert_eq!(debug_of(&builder), expected);
        assert_eq!(
            debug_of(&NtLoggerBuilder::new()),
            "NtLoggerBuilder { log_level: Info, debug_logger: None, sinks: [] }"
        );
    }

    #[test]
    fn a_logger_shows_its_debugger_sink_and_a_placeholder_per_sink() {
        static EVERY: Quiet = Quiet(LevelFilter::Trace);
        static SINKS: [&dyn Sink; 1] = [&EVERY];

        let logger = NtLoggerBuilder::new()
            .with_debug_logger()
            .with_sinks(&SINKS)
            .build();

        let mut expected = heapless::String::<512>::new();
        write!(
            expected,
            "NtLogger {{ log_level: Info, debugger: Some(DebuggerLogger {{ options: {:?}, skipped_above_dirql: 0 }}), sinks: [<dyn Sink>] }}",
            DebugLoggerOptions::new()
        )
        .unwrap();

        assert_eq!(debug_of(&logger), expected);
    }

    #[test]
    fn a_cloned_builder_keeps_every_setting() {
        static EVERY: Quiet = Quiet(LevelFilter::Trace);
        static SINKS: [&dyn Sink; 1] = [&EVERY];

        let builder = NtLoggerBuilder::new()
            .with_log_level(LevelFilter::Debug)
            .with_debug_logger_options(
                DebugLoggerOptions::new().with_component(DbgPrintComponent::IHV_BUS),
            )
            .with_sinks(&SINKS);

        let clone = builder.clone();

        // `Debug` shows the level and the debugger's options, but the sinks only by number.
        assert_eq!(debug_of(&clone), debug_of(&builder));
        assert!(ptr::eq(clone.sinks, builder.sinks));
    }

    #[test]
    fn the_options_can_be_hashed() {
        // A driver may key a map with them, and only this crate can implement `Hash` for them.
        fn hashable<T: Hash + Eq>() {}

        hashable::<DbgPrintComponent>();
        hashable::<DbgPrintLevel>();
        hashable::<DebugLoggerOptions>();
    }
}
