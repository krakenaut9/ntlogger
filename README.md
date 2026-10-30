# ntlogger

[![CI](https://github.com/krakenaut9/ntlogger/actions/workflows/ci.yml/badge.svg)](https://github.com/krakenaut9/ntlogger/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/ntlogger.svg)](https://crates.io/crates/ntlogger)
[![docs.rs](https://docs.rs/ntlogger/badge.svg)](https://docs.rs/ntlogger)
[![MSRV](https://img.shields.io/badge/rustc-1.88+-blue.svg)](#minimum-supported-rust-version)

A logger for Windows kernel-mode Rust drivers.

`ntlogger` is a logger for the [`log`](https://docs.rs/log) crate, so a driver logs with
`log`'s macros, `error!` to `trace!`. It writes each record as one line to the kernel
debugger, through `DbgPrintEx`, and to sinks of the driver's own. It is `#![no_std]`,
never allocates, and takes records at any IRQL.

---

## Highlights

- **One line per record**, with the time in UTC to the microsecond, the level, the IDs of
  the process and the thread, the target and the message.
- **Long lines are cut, not lost**: `DbgPrintEx` sends at most 512 bytes per call, so a
  longer line is cut at a character boundary and ends with `...`.
- **Any IRQL**: records are taken at any IRQL. Above DIRQL, where `DbgPrintEx` can
  deadlock, the debugger sink counts them instead of printing them.
- **Two filters**: `log`'s level decides which records are logged at all, and the
  debugger's filter masks decide which of them it shows.
- **Sinks of your own**, each with a level of its own, written to after the debugger.
- **No allocation**: each line is built on the stack, and the sinks are a `'static` list.

---

## Usage

Add the crate, and `log`, to your driver:

```toml
[dependencies]
log = "0.4"
ntlogger = "0.1"
```

Install the logger once, in `DriverEntry`, then log with `log`'s macros anywhere in the
driver:

```rust,no_run
use log::LevelFilter;
use ntlogger::NtLoggerBuilder;

fn setup_logger() -> Result<(), log::SetLoggerError> {
    NtLoggerBuilder::new()
        .with_debug_logger()
        .with_log_level(LevelFilter::Info)
        .build()
        .init()
}

fn on_start() {
    log::info!("Driver started");
}
```

`init` fails only if a logger is already installed. That logger stays in use, and the new
one reports the failure to its own sinks before it is dropped.

A driver with a logger of its own can nest this one in it instead: `build` returns an
`NtLogger`, which implements `log::Log`. A nested logger takes the records within `log`'s
level, which the driver then sets itself with `log::set_max_level`.

---

## The line

```text
2026-05-19 10:29:12.345678Z INFO  [4|1234] [my_driver::driver] Driver started
```

- The time is the system time, in UTC to the microsecond. A time outside the years 0 to
  9999 shows as `????-??-?? ??:??:??.??????Z`.
- The level is padded to five characters, so that the lines align.
- The IDs are those of the process and the thread that logged the record. In a DPC or an
  ISR, they are those of the thread it interrupted.
- Then come the record's target, by default its module path, and the message.

A line takes at most 511 bytes with its newline, which `DbgPrintEx` prints whole. A longer
one is cut at a character boundary and ends with `...`. Text that holds a NUL is printed
only up to the NUL, then the newline. The text goes to the debugger as UTF-8 bytes, so a
debugger that reads them in a code page such as Windows-1252 shows `é` as `Ã©`.

---

## Seeing the output

By default, the debugger sink prints under the component `IHVDRIVER`, at the `DbgPrintEx`
level `ERROR`, which a kernel debugger always shows. Any other level is shown only once
the component's filter mask enables it:

- in `WinDbg` or KD, `ed nt!Kd_IHVDRIVER_Mask 0x8` enables `INFO` at once, until Windows
  restarts;
- a DWORD value named `IHVDRIVER` under
  `HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Debug Print Filter` sets the mask
  from the next boot.

Without a debugger, Sysinternals
[`DebugView`](https://learn.microsoft.com/en-us/sysinternals/downloads/debugview) shows the
output once its kernel capture is enabled.

`DebugLoggerOptions` sets the component, the `DbgPrintEx` level, and a `log` level of the
debugger's own:

```rust,no_run
use ntlogger::{DbgPrintComponent, DbgPrintLevel, DebugLoggerOptions, NtLoggerBuilder};

/// A network driver whose records are shown once the debugger enables `INFO` for its
/// component: `ed nt!Kd_IHVNETWORK_Mask 0x8`.
fn setup_logger() -> Result<(), log::SetLoggerError> {
    NtLoggerBuilder::new()
        .with_debug_logger_options(
            DebugLoggerOptions::new()
                .with_component(DbgPrintComponent::IHV_NETWORK)
                .with_dbgprint_level(DbgPrintLevel::INFO),
        )
        .build()
        .init()
}
```

Microsoft's
[Reading and Filtering Debugging Messages](https://learn.microsoft.com/en-us/windows-hardware/drivers/debugger/reading-and-filtering-debugging-messages)
explains the components, the levels and the masks.

---

## Levels

`with_log_level` sets the logger's level, `Info` unless set. `init` gives it to `log`, so
`log::set_max_level` changes it while the driver runs, from an IOCTL handler for example.
It is a single atomic store, so any IRQL will do.

`log` can also remove levels at compile time. With this in the driver's `Cargo.toml`,
release builds compile `debug!` and `trace!` to nothing:

```toml
log = { version = "0.4", features = ["release_max_level_info"] }
```

A sink's own level, `Sink::max_level` or `DebugLoggerOptions::with_max_level`, can only
lower the logger's.

---

## Sinks of your own

A sink is a `static` whose type implements `Sink`. The logger writes to the sinks given to
`with_sinks` after the debugger, in their order:

```rust,no_run
use core::sync::atomic::{AtomicUsize, Ordering};
use log::LevelFilter;
use ntlogger::{Entry, NtLoggerBuilder, Sink};

/// Counts the errors.
struct ErrorCounter(AtomicUsize);

impl Sink for ErrorCounter {
    fn max_level(&self) -> LevelFilter {
        LevelFilter::Error
    }

    fn write(&self, _entry: &Entry<'_>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

static ERRORS: ErrorCounter = ErrorCounter(AtomicUsize::new(0));
static SINKS: [&dyn Sink; 1] = [&ERRORS];

fn setup_logger() -> Result<(), log::SetLoggerError> {
    NtLoggerBuilder::new()
        .with_debug_logger()
        .with_sinks(&SINKS)
        .build()
        .init()
}
```

An `Entry` gives a sink the record, the IDs of the process and the thread, the IRQL and
the time, and its `Display` form is the line above.

---

## IRQL

- **Logging** works at any IRQL, in DPCs and ISRs, and on several processors at once. The
  logger reads the IDs, the IRQL and the time only for a record that some sink takes, with
  functions that run at any IRQL.
- **The debugger sink** prints up to DIRQL. Above it, where `DbgPrintEx` can deadlock, it
  counts the record instead, and `NtLogger::debug_logger_skipped_above_dirql` returns how
  many.
- **A sink of your own** is called at the IRQL of the log call. It must not allocate, wait,
  touch pageable code or data, take a lock that the code it interrupted may hold, or log.
  One that cannot work at every IRQL checks `Entry::irql` and skips what it cannot handle.
- **`init`** works at any IRQL. A driver calls it once, in `DriverEntry`.
- **The builder and the options** are plain values.

---

## Requirements

- A Windows kernel-mode driver for `x86_64-pc-windows-msvc`, `i686-pc-windows-msvc` or
  `aarch64-pc-windows-msvc`
- Rust 1.88 or later

The logger imports `DbgPrintEx`, `KeQuerySystemTimePrecise`, `PsGetCurrentProcessId` and
`PsGetCurrentThreadId` from `ntoskrnl.exe`, and `KeGetCurrentIrql` from `ntoskrnl.exe` on
x64 and from `hal.dll` on x86 and ARM64. It declares them with `raw-dylib`, so building it
needs no import library and no WDK.

---

## Minimum supported Rust version

**1.88**, which current releases of the `time` crate need.

The MSRV is treated as a compatibility promise: raising it is a breaking change and will
come with a minor version bump before 1.0.

---

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
