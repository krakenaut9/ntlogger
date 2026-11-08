# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

While the version is below 1.0, a minor bump may contain breaking changes.

## [Unreleased]

## [0.1.0] - 2026-11-08

First public release.

### Added

- `NtLogger`, a logger for the `log` crate in `#![no_std]` Windows kernel-mode
  drivers on x64, x86 and ARM64. It writes each record as one line, with the
  time in UTC to the microsecond, the level, the IDs of the process and the
  thread, the target and the message, to the kernel debugger and to sinks of
  the driver's own. It never allocates and takes records at any IRQL.
- `NtLoggerBuilder`, which sets the logger up with `with_log_level`,
  `with_debug_logger`, `with_debug_logger_options` and `with_sinks`, then
  `build`s it. `NtLogger::init` installs the logger and gives its level to
  `log`, so `log::set_max_level` changes it while the driver runs. A driver
  with a logger of its own can nest the built `NtLogger` in it instead. A
  nested logger takes the records within `log`'s level, which the driver then
  sets itself with `log::set_max_level`.
- The kernel debugger sink, which prints each line with `DbgPrintEx`. A line
  too long for the 512 bytes `DbgPrintEx` sends at once is cut at a character
  boundary and ends with `...`. Above DIRQL, where `DbgPrintEx` can deadlock,
  the sink counts the record instead, and
  `NtLogger::debug_logger_skipped_above_dirql` returns how many.
- `DebugLoggerOptions`, to choose the sink's `DbgPrintEx` component and level,
  and a `log` level of its own. The default, `DbgPrintComponent::IHV_DRIVER` at
  `DbgPrintLevel::ERROR`, is shown without any setup.
- `DbgPrintComponent` and `DbgPrintLevel`, which name the components for
  drivers, `IHV_DRIVER` to `IHV_BUS`, and the four standard levels.
  `DbgPrintLevel::bit` and `DbgPrintLevel::mask` build a level of the driver's
  own, `from_raw` takes either value as `DbgPrintEx` does, and
  `importance_bits` gives the filter mask bits that show a level.
- The `Sink` trait, for sinks of the driver's own. Each has a level of its own
  and is written to after the debugger, in the order given to `with_sinks`. A
  sink gets an `Entry`: the record, the IDs of the process and the thread, the
  IRQL and the time, with the line as its `Display` form.

### Notes

- `init` fails only if a logger is already installed. That logger stays in
  use, and the new one reports the failure to its own sinks.
- The debugger gets the text as UTF-8 bytes, and prints a line that holds a NUL
  only up to the NUL.
- The logger imports `DbgPrintEx`, `KeQuerySystemTimePrecise`,
  `PsGetCurrentProcessId` and `PsGetCurrentThreadId` from `ntoskrnl.exe`, and
  `KeGetCurrentIrql` from `ntoskrnl.exe` on x64 and from `hal.dll` on x86 and
  ARM64. Its imports use `raw-dylib`, so building needs no import library and
  no WDK.
- Minimum supported Rust version is 1.88.

[Unreleased]: https://github.com/krakenaut9/ntlogger/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/krakenaut9/ntlogger/releases/tag/v0.1.0
