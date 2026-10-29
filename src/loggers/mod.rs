pub mod dbgprint;
pub mod debug;

use crate::time_util::{TIMESTAMP_LEN, Timestamp};
use core::fmt::{self, Write};
use heapless::String;
use log::{Level, LevelFilter, Record};

/// A destination for records.
///
/// The kernel debugger is one, built in and enabled with
/// [`with_debug_logger`](crate::NtLoggerBuilder::with_debug_logger). A driver gives sinks
/// of its own to [`with_sinks`](crate::NtLoggerBuilder::with_sinks).
///
/// The logger calls a sink where the record is logged: at any IRQL, in DPCs and ISRs, and
/// on several processors at once. So a sink must not allocate, wait, touch pageable code or
/// data, or take a lock that the code it interrupted may hold. Nor may it log, which would
/// call it again. A sink that cannot work at every IRQL checks [`Entry::irql`] and skips
/// what it cannot handle, as the kernel debugger sink does above DIRQL.
///
/// ```no_run
/// use core::sync::atomic::{AtomicUsize, Ordering};
/// use log::LevelFilter;
/// use ntlogger::{Entry, NtLoggerBuilder, Sink};
///
/// /// Counts the errors.
/// struct ErrorCounter(AtomicUsize);
///
/// impl Sink for ErrorCounter {
///     fn max_level(&self) -> LevelFilter {
///         LevelFilter::Error
///     }
///
///     fn write(&self, _entry: &Entry<'_>) {
///         self.0.fetch_add(1, Ordering::Relaxed);
///     }
/// }
///
/// static ERRORS: ErrorCounter = ErrorCounter(AtomicUsize::new(0));
/// static SINKS: [&dyn Sink; 1] = [&ERRORS];
///
/// NtLoggerBuilder::new()
///     .with_debug_logger()
///     .with_sinks(&SINKS)
///     .build()
///     .init()?;
/// # Ok::<(), log::SetLoggerError>(())
/// ```
pub trait Sink: Sync {
    /// The most verbose records this sink takes: [`LevelFilter::Trace`], the default, takes
    /// every record the logger's level
    /// ([`with_log_level`](crate::NtLoggerBuilder::with_log_level)) lets through, and
    /// [`LevelFilter::Off`] none. No sink takes a record above the logger's level, whatever
    /// this returns.
    ///
    /// It is read for every record within the logger's level, so a sink may change it at any
    /// time.
    fn max_level(&self) -> LevelFilter {
        LevelFilter::Trace
    }

    /// Writes `entry`, a record at [`max_level`](Self::max_level) or below.
    fn write(&self, entry: &Entry<'_>);

    /// Writes out whatever the sink still holds: called by `log::logger().flush()`. The
    /// default does nothing.
    fn flush(&self) {}
}

/// A record as a sink receives it: the record, and what the logger read from the kernel
/// when it was logged.
///
/// Its [`Display`](fmt::Display) form is the record as one line of text, without a newline:
/// the time in UTC to the microsecond, the level, the IDs of the process and the thread, the
/// target and the message.
///
/// ```text
/// 2026-05-19 10:29:12.345678Z INFO  [4|1234] [my_driver::driver] Driver started
/// ```
///
/// A time outside the years 0 to 9999 shows as `????-??-?? ??:??:??.??????Z`. In a DPC or an
/// ISR, the IDs are those of the thread it interrupted.
///
/// A sink of the driver's own takes what it needs from the entry:
///
/// ```
/// use core::sync::atomic::{AtomicU8, Ordering};
/// use log::LevelFilter;
/// use ntlogger::{Entry, Sink};
///
/// /// Remembers the IRQL of the last error.
/// struct LastErrorIrql(AtomicU8);
///
/// impl Sink for LastErrorIrql {
///     fn max_level(&self) -> LevelFilter {
///         LevelFilter::Error
///     }
///
///     fn write(&self, entry: &Entry<'_>) {
///         self.0.store(entry.irql(), Ordering::Relaxed);
///     }
/// }
/// ```
#[derive(Debug, Clone, Copy)]
pub struct Entry<'a> {
    record: &'a Record<'a>,
    process_id: usize,
    thread_id: usize,
    irql: u8,
    system_time: i64,
}

impl<'a> Entry<'a> {
    pub(crate) const fn new(
        record: &'a Record<'a>,
        process_id: usize,
        thread_id: usize,
        irql: u8,
        system_time: i64,
    ) -> Self {
        Self {
            record,
            process_id,
            thread_id,
            irql,
            system_time,
        }
    }

    /// The record: its level, target, message and the rest.
    #[must_use]
    pub const fn record(&self) -> &'a Record<'a> {
        self.record
    }

    /// The ID of the process that owns the thread that logged the record, even while that
    /// thread is attached to another process. In a DPC or an ISR, the thread is whichever
    /// one it interrupted.
    #[must_use]
    pub const fn process_id(&self) -> usize {
        self.process_id
    }

    /// The ID of the thread that logged the record. In a DPC or an ISR, that is whichever
    /// thread it interrupted.
    #[must_use]
    pub const fn thread_id(&self) -> usize {
        self.thread_id
    }

    /// The IRQL the record was logged at, as `KeGetCurrentIrql` returns it.
    ///
    /// Above `DISPATCH_LEVEL`, the levels depend on the architecture: `HIGH_LEVEL`, for one,
    /// is 31 on x86 and 15 on x64 and ARM64. So compare it with values made for the target,
    /// such as those wdk-sys generates from the WDK's headers. windows-sys 0.61 gives
    /// `CLOCK_LEVEL`, `PROFILE_LEVEL`, `IPI_LEVEL`, `POWER_LEVEL` and `HIGH_LEVEL` their x86
    /// values on every architecture.
    #[must_use]
    pub const fn irql(&self) -> u8 {
        self.irql
    }

    /// When the record was logged, as `KeQuerySystemTimePrecise` gives it: UTC, in
    /// 100-nanosecond intervals since 1601-01-01.
    #[must_use]
    pub const fn system_time(&self) -> i64 {
        self.system_time
    }
}

impl fmt::Display for Entry<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The line is written by hand rather than with the obvious
        // `write!(f, "{} {:<5} [{}|{}] [{}] {}", time, level, pid, tid, target, message)`,
        // because that is slower: each `{}` goes through `core::fmt`, a call through a function
        // pointer followed by checks for width and alignment, and each piece that reaches `f` is
        // a call into the sink's writer. So everything up to the target,
        // `2026-05-19 10:29:12.345678Z INFO  [4|1234] [`, is put together on the stack, with the
        // numbers written by hand (`Timestamp`, `Decimal`) and the level's name taken from a
        // table (`padded_level_name`), and handed to `f` in one piece. Measured in release
        // builds, a typical line takes about a third less time.
        self.write_head(f)?;
        f.write_str(self.record.target())?;
        f.write_str("] ")?;

        f.write_fmt(*self.record.args())
    }
}

/// The longest head a line can have: a timestamp, a level name as long as the longest one,
/// `ERROR`, and both IDs at their longest, with what separates them.
const HEAD_CAPACITY: usize = TIMESTAMP_LEN
    + " ".len()
    + "ERROR".len()
    + " [".len()
    + USIZE_DIGITS
    + "|".len()
    + USIZE_DIGITS
    + "] [".len();

impl Entry<'_> {
    /// Writes the line up to its target to `f`, in one piece. Not inlined, so that its buffers
    /// are off the stack by the time the message is formatted, where the stack usually goes
    /// deepest.
    #[inline(never)]
    fn write_head(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.head()?.as_str())
    }

    /// The line up to its target: `2026-05-19 10:29:12.345678Z INFO  [4|1234] [`.
    fn head(&self) -> Result<String<HEAD_CAPACITY>, fmt::Error> {
        let mut head = String::new();

        for piece in [
            Timestamp::new(self.system_time).as_str(),
            " ",
            padded_level_name(self.record.level()),
            " [",
            Decimal::new(self.process_id).as_str(),
            "|",
            Decimal::new(self.thread_id).as_str(),
            "] [",
        ] {
            head.write_str(piece)?;
        }

        Ok(head)
    }
}

/// The name of `level`, padded with spaces to the length of the longest, so that what follows
/// it lines up from one line to the next. A table is faster than `{:<5}`, which measures the
/// name and writes the padding one space at a time.
const fn padded_level_name(level: Level) -> &'static str {
    match level {
        Level::Error => "ERROR",
        Level::Warn => "WARN ",
        Level::Info => "INFO ",
        Level::Debug => "DEBUG",
        Level::Trace => "TRACE",
    }
}

/// The most digits a `usize` has: 20 on 64-bit targets, 10 on 32-bit ones.
const USIZE_DIGITS: usize = usize::MAX.ilog10() as usize + 1;

/// A `usize` as decimal text, `1234`, written by hand: `{}` is slower, since it goes through
/// `core::fmt`.
struct Decimal {
    digits: [u8; USIZE_DIGITS],
    /// Where the digits start: they fill the end of `digits`.
    start: usize,
}

impl Decimal {
    /// The digits of `value`.
    fn new(mut value: usize) -> Self {
        let mut digits = [0; USIZE_DIGITS];
        let mut start = USIZE_DIGITS;

        // From the last digit back, until the value has no digits left: always at least one,
        // so that 0 is written as `0`.
        for (index, digit) in digits.iter_mut().enumerate().rev() {
            // `value % 10` is below 10, so it always fits.
            *digit = b'0' + u8::try_from(value % 10).unwrap_or_default();
            start = index;

            value /= 10;
            if value == 0 {
                break;
            }
        }

        Self { digits, start }
    }

    /// The number as text: it is all ASCII.
    fn as_str(&self) -> &str {
        core::str::from_utf8(self.digits.get(self.start..).unwrap_or_default()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::{iter, ptr};

    /// 1970-01-01 00:00:00 UTC, in system time.
    const UNIX_EPOCH: i64 = 116_444_736_000_000_000;
    /// The IRQL DPCs run at.
    const DISPATCH_LEVEL: u8 = 2;

    #[test]
    fn a_sinks_level_is_trace_unless_it_sets_one() {
        /// A sink that keeps the default level.
        struct Plain;

        impl Sink for Plain {
            fn write(&self, _entry: &Entry<'_>) {}
        }

        assert_eq!(Plain.max_level(), LevelFilter::Trace);
    }

    #[test]
    fn an_entry_returns_what_the_logger_read() {
        let record = Record::builder().build();
        let entry = Entry::new(&record, 1234, 5678, DISPATCH_LEVEL, UNIX_EPOCH);

        assert!(ptr::eq(entry.record(), &raw const record));
        assert_eq!(
            (
                entry.process_id(),
                entry.thread_id(),
                entry.irql(),
                entry.system_time()
            ),
            (1234, 5678, DISPATCH_LEVEL, UNIX_EPOCH)
        );
    }

    #[test]
    fn an_entry_displays_as_the_log_line() {
        let record = Record::builder()
            .args(format_args!("Test log message"))
            .level(Level::Info)
            .target("test_target")
            .build();
        let entry = Entry::new(&record, 1234, 5678, DISPATCH_LEVEL, UNIX_EPOCH);

        let mut line = String::<512>::new();
        write!(line, "{entry}").unwrap();

        assert_eq!(
            line.as_str(),
            "1970-01-01 00:00:00.000000Z INFO  [1234|5678] [test_target] Test log message"
        );
    }

    #[test]
    fn the_longest_head_fills_its_capacity() {
        // Every timestamp and every level name has the same length, so only the IDs vary.
        let record = Record::builder().build();
        let entry = Entry::new(&record, usize::MAX, usize::MAX, DISPATCH_LEVEL, UNIX_EPOCH);

        assert_eq!(entry.head().map(|head| head.len()), Ok(HEAD_CAPACITY));
    }

    #[test]
    fn each_level_name_is_padded_to_five_characters() {
        for level in Level::iter() {
            let mut expected = String::<8>::new();
            write!(expected, "{:<5}", level.as_str()).unwrap();

            assert_eq!(padded_level_name(level), expected.as_str());
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "takes most of a minute under Miri, and has no unsafe code to check"
    )]
    fn a_decimal_is_what_write_makes_of_the_number() {
        // Each power of ten and the numbers on either side of it, 0 among them, the two
        // largest numbers, and numbers picked by a xorshift generator.
        let around_powers_of_ten = iter::successors(Some(1_usize), |power| power.checked_mul(10))
            .flat_map(|power| [power - 1, power, power + 1]);
        let largest = [usize::MAX - 1, usize::MAX];
        let usize_max = u64::try_from(usize::MAX).unwrap();
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let picked = iter::repeat_with(move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;

            // Within range on 32-bit targets too.
            usize::try_from(state % usize_max).unwrap()
        })
        .take(2_000);

        for value in around_powers_of_ten.chain(largest).chain(picked) {
            let mut expected = String::<32>::new();
            write!(expected, "{value}").unwrap();

            assert_eq!(Decimal::new(value).as_str(), expected.as_str(), "{value}");
        }
    }
}
