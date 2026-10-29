use crate::loggers::{
    Entry, Sink,
    dbgprint::{DbgPrintComponent, DbgPrintLevel},
};
use core::{
    ffi::CStr,
    fmt::{self, Write},
    sync::atomic::{AtomicUsize, Ordering},
};
use heapless::String;
use log::LevelFilter;

// windows-sys imports DbgPrintEx from ntdll.dll, which a driver cannot load.
windows_link::link!("ntoskrnl.exe" "C" fn DbgPrintEx(componentid: u32, level: u32, format: *const u8, ...) -> u32);

/// How the kernel debugger logger handles records: which records it takes, and under which
/// `DbgPrintEx` component ID and at which `DbgPrintEx` level it prints them.
///
/// By default it takes every record the logger's level lets through, and prints it under
/// [`DbgPrintComponent::IHV_DRIVER`] at [`DbgPrintLevel::ERROR`], which a kernel debugger
/// always shows. A record at any other `DbgPrintEx` level is shown only once its
/// component's filter mask enables it; see [`DbgPrintLevel`].
///
/// ```
/// use log::LevelFilter;
/// use ntlogger::{DbgPrintComponent, DbgPrintLevel, DebugLoggerOptions};
///
/// // A network driver, whose records are shown once the debugger enables INFO for its
/// // component: `ed nt!Kd_IHVNETWORK_Mask 0x8`. It takes no trace records, even when the
/// // logger's level lets them through.
/// let options = DebugLoggerOptions::new()
///     .with_component(DbgPrintComponent::IHV_NETWORK)
///     .with_dbgprint_level(DbgPrintLevel::INFO)
///     .with_max_level(LevelFilter::Debug);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DebugLoggerOptions {
    max_level: LevelFilter,
    component: DbgPrintComponent,
    dbgprint_level: DbgPrintLevel,
}

impl DebugLoggerOptions {
    /// The defaults: every record the logger's level lets through,
    /// [`DbgPrintComponent::IHV_DRIVER`] and [`DbgPrintLevel::ERROR`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_level: LevelFilter::Trace,
            component: DbgPrintComponent::IHV_DRIVER,
            dbgprint_level: DbgPrintLevel::ERROR,
        }
    }

    /// Takes only the records up to `level`, and never one above the logger's level
    /// ([`NtLoggerBuilder::with_log_level`](crate::NtLoggerBuilder::with_log_level)),
    /// whatever `level` is. With [`LevelFilter::Off`] it takes none.
    #[must_use]
    pub const fn with_max_level(mut self, level: LevelFilter) -> Self {
        self.max_level = level;
        self
    }

    /// Prints records under `component`.
    #[must_use]
    pub const fn with_component(mut self, component: DbgPrintComponent) -> Self {
        self.component = component;
        self
    }

    /// Prints records at the `DbgPrintEx` level `level`.
    #[must_use]
    pub const fn with_dbgprint_level(mut self, level: DbgPrintLevel) -> Self {
        self.dbgprint_level = level;
        self
    }
}

impl Default for DebugLoggerOptions {
    fn default() -> Self {
        Self::new()
    }
}

// DbgPrintEx is documented for IRQL <= DIRQL and can deadlock above it, since the debugger
// uses interprocessor interrupts. This is the first IRQL above every device IRQL in wdm.h:
// PROFILE_LEVEL on x86, CLOCK_LEVEL on every other architecture. windows-sys's constants of
// those names have their x86 values on every architecture, so they cannot be used here.
#[cfg(target_arch = "x86")]
const FIRST_IRQL_ABOVE_DIRQL: u8 = 27;
#[cfg(not(target_arch = "x86"))]
const FIRST_IRQL_ABOVE_DIRQL: u8 = 13;

/// The size of the buffer `DbgPrintEx` formats each call into: it transmits at most this
/// many bytes per call.
const DBGPRINTEX_BUFFER_SIZE: usize = 512;
/// The most a printed line may take, its newline included. Windows does not document whether
/// the buffer's last byte can hold text or is kept for the NUL that ends it, and the `ReactOS`
/// implementation keeps it for the NUL. A line that lost its last byte would lose its newline
/// and run the next record onto it, so that byte is left free.
const LINE_CAPACITY: usize = DBGPRINTEX_BUFFER_SIZE - 1;
/// The newline [`FORMAT`] prints after every line, since `DbgPrintEx` adds none of its own.
const NEWLINE: &str = "\n";
/// The format `DbgPrintEx` prints a line with: the line, then the newline. The newline is part
/// of the format so that it is printed even when `%.*s` stops early, at a NUL in the line.
const FORMAT: &CStr = c"%.*s\n";
/// Ends the text of a line that was cut.
const CUT_MARKER: &str = "...";
/// The room for text: what is left once the marker and the newline are set aside, so that
/// both always fit after it.
const TEXT_CAPACITY: usize = LINE_CAPACITY - CUT_MARKER.len() - NEWLINE.len();

/// One line for `DbgPrintEx`, without the newline that [`FORMAT`] adds. Text that does not
/// fit is cut at a character boundary, and the line then ends with [`CUT_MARKER`].
struct Line {
    text: String<{ TEXT_CAPACITY + CUT_MARKER.len() }>,
    /// Whether the text was cut: where the next text did not fit, or where a value failed to
    /// format.
    cut: bool,
}

impl Line {
    /// An empty line, to be filled in place by [`write_entry`](Self::write_entry). A
    /// constructor that returned a filled line would copy all of it: the compiler does not
    /// build a value in the caller's frame once `write!` has borrowed it.
    const fn new() -> Self {
        Self {
            text: String::new(),
            cut: false,
        }
    }

    /// Writes the line for `entry` into this empty line: its text, and the marker if the text
    /// was cut.
    fn write_entry(&mut self, entry: &Entry<'_>) {
        // Formatting stops at the first error: the one `write_str` returns when it cuts the
        // text, or one from a value that failed to format. Either way, the rest is missing.
        if write!(self, "{entry}").is_err() {
            self.cut = true;
        }

        // TEXT_CAPACITY leaves room for the marker, so the push does not fail.
        if self.cut {
            let _ = self.text.push_str(CUT_MARKER);
        }
    }
}

impl Write for Line {
    /// Appends `text` if it all fits. Otherwise appends the part that fits, cut at a
    /// character boundary, and returns an error, since `write_str` may succeed only when it
    /// wrote all of `text`. The error stops the formatting, so the rest of the record is not
    /// formatted only to be dropped, and `cut` tells `write_entry` what happened. After a cut,
    /// it appends nothing more.
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if self.cut {
            return Err(fmt::Error);
        }
        let room = TEXT_CAPACITY.saturating_sub(self.text.len());
        if text.len() <= room {
            return self.text.push_str(text).map_err(|_| fmt::Error);
        }
        // The last character boundary within the room. 0 is always one.
        let fits = (0..=room)
            .rev()
            .find(|&index| text.is_char_boundary(index))
            .unwrap_or_default();
        let _ = self.text.push_str(text.get(..fits).unwrap_or_default());
        self.cut = true;
        Err(fmt::Error)
    }
}

/// Writes records to the kernel debugger with `DbgPrintEx`.
#[derive(Debug)]
pub(crate) struct DebuggerLogger {
    options: DebugLoggerOptions,
    /// How many records it has not printed because they were logged above DIRQL.
    skipped_above_dirql: AtomicUsize,
}

impl DebuggerLogger {
    pub(crate) const fn new(options: DebugLoggerOptions) -> Self {
        Self {
            options,
            skipped_above_dirql: AtomicUsize::new(0),
        }
    }

    /// Whether a record logged at `irql` must be skipped: above DIRQL, where `DbgPrintEx`
    /// can deadlock. Counts the records it skips.
    fn skip_above_dirql(&self, irql: u8) -> bool {
        let skip = irql >= FIRST_IRQL_ABOVE_DIRQL;
        if skip {
            // Only the count matters, so Relaxed ordering is enough.
            self.skipped_above_dirql.fetch_add(1, Ordering::Relaxed);
        }
        skip
    }

    /// How many records it has not printed because they were logged above DIRQL.
    pub(crate) fn skipped_above_dirql(&self) -> usize {
        self.skipped_above_dirql.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) const fn options(&self) -> DebugLoggerOptions {
        self.options
    }
}

impl Sink for DebuggerLogger {
    fn max_level(&self) -> LevelFilter {
        self.options.max_level
    }

    fn write(&self, entry: &Entry<'_>) {
        if self.skip_above_dirql(entry.irql()) {
            return;
        }

        let mut line = Line::new();
        line.write_entry(entry);

        let len = i32::try_from(line.text.len()).unwrap_or(i32::MAX);
        // SAFETY: `FORMAT` is a literal, so a `%` in the line is printed as is. Its `%.*s` reads
        // at most `len` bytes from the line, which holds that many.
        unsafe {
            DbgPrintEx(
                self.options.component.raw(),
                self.options.dbgprint_level.raw(),
                FORMAT.as_ptr().cast(),
                len,
                line.text.as_ptr(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::{cell::Cell, str};
    use log::{Level, Record};

    /// 1970-01-01 00:00:00 UTC, in system time.
    const UNIX_EPOCH: i64 = 116_444_736_000_000_000;

    // IRQLs from wdm.h: the one DPCs run at, the highest a device interrupt runs at, and the
    // highest of all.
    const DISPATCH_LEVEL: u8 = 2;
    #[cfg(target_arch = "x86")]
    const HIGHEST_DIRQL: u8 = 26;
    #[cfg(target_arch = "x86")]
    const HIGH_LEVEL: u8 = 31;
    #[cfg(not(target_arch = "x86"))]
    const HIGHEST_DIRQL: u8 = 12;
    #[cfg(not(target_arch = "x86"))]
    const HIGH_LEVEL: u8 = 15;

    /// Enough `x` for any message below.
    static X_BYTES: [u8; 10 * 1024] = [b'x'; 10 * 1024];

    /// `len` times `x`.
    fn xs(len: usize) -> &'static str {
        str::from_utf8(&X_BYTES[..len]).unwrap()
    }

    /// The line for an info record with `message`.
    fn line_for(message: fmt::Arguments<'_>) -> Line {
        let mut line = Line::new();
        line.write_entry(&Entry::new(
            &Record::builder()
                .args(message)
                .level(Level::Info)
                .target("test_target")
                .build(),
            1234,
            5678,
            DISPATCH_LEVEL,
            UNIX_EPOCH,
        ));

        line
    }

    /// How long a line is before its message.
    fn prefix_len() -> usize {
        line_for(format_args!("")).text.len()
    }

    /// What every line must do: fit in `LINE_CAPACITY` with the newline `FORMAT` adds, so that
    /// `DbgPrintEx` prints it whole.
    fn assert_fits(line: &Line) {
        assert!(line.text.len() + NEWLINE.len() <= LINE_CAPACITY);
    }

    #[test]
    fn the_options_start_at_the_defaults_and_follow_the_setters() {
        let defaults = DebugLoggerOptions::new();
        assert_eq!(DebugLoggerOptions::default(), defaults);
        assert_eq!(
            (
                defaults.max_level,
                defaults.component,
                defaults.dbgprint_level
            ),
            (
                LevelFilter::Trace,
                DbgPrintComponent::IHV_DRIVER,
                DbgPrintLevel::ERROR
            )
        );

        let options = DebugLoggerOptions::new()
            .with_max_level(LevelFilter::Debug)
            .with_component(DbgPrintComponent::IHV_BUS)
            .with_dbgprint_level(DbgPrintLevel::TRACE);
        assert_eq!(
            (options.max_level, options.component, options.dbgprint_level),
            (
                LevelFilter::Debug,
                DbgPrintComponent::IHV_BUS,
                DbgPrintLevel::TRACE
            )
        );
    }

    #[test]
    fn the_debugger_takes_the_level_of_its_options() {
        let default = DebuggerLogger::new(DebugLoggerOptions::new());
        assert_eq!(default.max_level(), LevelFilter::Trace);

        let own = DebuggerLogger::new(DebugLoggerOptions::new().with_max_level(LevelFilter::Warn));
        assert_eq!(own.max_level(), LevelFilter::Warn);
    }

    #[test]
    fn records_above_dirql_are_counted_not_printed() {
        let debugger = DebuggerLogger::new(DebugLoggerOptions::new());
        assert!(!debugger.skip_above_dirql(DISPATCH_LEVEL));
        assert!(!debugger.skip_above_dirql(HIGHEST_DIRQL));
        assert!(debugger.skip_above_dirql(HIGHEST_DIRQL + 1));
        assert!(debugger.skip_above_dirql(HIGH_LEVEL));
        assert_eq!(debugger.skipped_above_dirql(), 2);
    }

    #[test]
    fn a_short_line_is_written_whole() {
        let line = line_for(format_args!("Test log message"));

        assert_eq!(
            line.text,
            "1970-01-01 00:00:00.000000Z INFO  [1234|5678] [test_target] Test log message"
        );
        assert!(!line.cut);
    }

    #[test]
    fn the_format_prints_the_line_then_the_newline() {
        assert_eq!(
            FORMAT.to_bytes().strip_suffix(NEWLINE.as_bytes()),
            Some(b"%.*s".as_slice())
        );
    }

    #[test]
    fn percent_signs_stay_as_they_are() {
        let line = line_for(format_args!("100% %s %n %ws"));

        assert!(line.text.ends_with("[test_target] 100% %s %n %ws"));
    }

    #[test]
    fn a_nul_stays_as_it_is() {
        // `DbgPrintEx` prints the line up to the NUL, then the newline of `FORMAT`, so the next
        // record still starts a line of its own.
        let line = line_for(format_args!("ab\0cd"));

        assert!(!line.cut);
        assert!(line.text.ends_with("[test_target] ab\0cd"));
    }

    #[test]
    fn a_line_that_fits_exactly_is_not_cut() {
        let line = line_for(format_args!("{}", xs(TEXT_CAPACITY - prefix_len())));

        assert_fits(&line);
        assert!(!line.cut);
        assert_eq!(line.text.len(), TEXT_CAPACITY);
        assert!(line.text.ends_with('x'));
    }

    #[test]
    fn one_byte_more_is_cut_and_marked() {
        let line = line_for(format_args!("{}", xs(TEXT_CAPACITY - prefix_len() + 1)));

        assert_fits(&line);
        assert!(line.cut);
        assert_eq!(line.text.len() + NEWLINE.len(), LINE_CAPACITY);
        assert!(line.text.ends_with("x..."));
    }

    #[test]
    fn a_character_across_the_limit_is_not_split() {
        for character in ['é', '€', '😀'] {
            let width = character.len_utf8();

            // Too little room drops the character whole, however little is missing.
            for room in 1..width {
                let line = line_for(format_args!(
                    "{}{character}",
                    xs(TEXT_CAPACITY - prefix_len() - room)
                ));

                assert_fits(&line);
                assert!(line.cut);
                assert_eq!(line.text.len() + NEWLINE.len(), LINE_CAPACITY - room);
                assert!(line.text.ends_with("x..."));
            }

            // Just enough room keeps it.
            let line = line_for(format_args!(
                "{}{character}",
                xs(TEXT_CAPACITY - prefix_len() - width)
            ));

            assert_fits(&line);
            assert!(!line.cut);
            assert!(line.text.ends_with(character));
        }
    }

    #[test]
    fn formatting_stops_at_the_cut() {
        /// Counts how often it is formatted.
        struct Counted<'a>(&'a Cell<usize>);

        impl fmt::Display for Counted<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.set(self.0.get() + 1);
                f.write_str("z")
            }
        }

        let formatted = Cell::new(0);
        let line = line_for(format_args!("{}{}", xs(10 * 1024), Counted(&formatted)));
        assert!(line.cut);
        // The cut came first, so the value after it was never formatted.
        assert_eq!(formatted.get(), 0);
    }

    #[test]
    fn a_10_kb_message_makes_one_line() {
        let line = line_for(format_args!("{}", xs(10 * 1024)));

        assert_fits(&line);
        assert!(line.cut);
        assert!(line.text.ends_with("x..."));
    }

    #[test]
    fn a_value_that_fails_to_format_ends_the_line_with_the_marker() {
        /// Writes part of itself, then fails.
        struct Fails;

        impl fmt::Display for Fails {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("half")?;
                Err(fmt::Error)
            }
        }

        let line = line_for(format_args!("before {Fails} after"));

        assert_fits(&line);
        assert!(line.cut);
        assert!(line.text.ends_with("[test_target] before half..."));
    }

    #[test]
    fn nothing_is_written_after_the_cut() {
        /// Writes "é", which does not fit, then ignores the error and goes on.
        struct IgnoresErrors;

        impl fmt::Display for IgnoresErrors {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let _ = f.write_str("é");
                f.write_str("a")
            }
        }

        // One byte is left: too few for "é", enough for "a".
        let line = line_for(format_args!(
            "{}{}",
            xs(TEXT_CAPACITY - prefix_len() - 1),
            IgnoresErrors
        ));

        assert_fits(&line);
        assert!(line.text.ends_with("x..."));
    }
}
