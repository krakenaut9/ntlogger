//! The two values `DbgPrintEx` filters records by: the component ID and the level.

use core::fmt;

/// Marks a `DbgPrintEx` level as bits rather than a bit number: `DPFLTR_MASK` in the
/// WDK's `dpfilter.h`.
const DPFLTR_MASK: u32 = 0x8000_0000;

/// A `DbgPrintEx` component ID: the component whose filter mask decides whether a record
/// is shown.
///
/// Microsoft asks drivers to use one of the six component IDs reserved for independent
/// hardware vendors, so that their output is not mixed with that of Windows components:
///
/// | Constant | In `dpfilter.h` | Value | Driver type |
/// |---|---|---|---|
/// | [`IHV_DRIVER`](Self::IHV_DRIVER) | `DPFLTR_IHVDRIVER_ID` | 77 | any other type of driver |
/// | [`IHV_VIDEO`](Self::IHV_VIDEO) | `DPFLTR_IHVVIDEO_ID` | 78 | video |
/// | [`IHV_AUDIO`](Self::IHV_AUDIO) | `DPFLTR_IHVAUDIO_ID` | 79 | audio |
/// | [`IHV_NETWORK`](Self::IHV_NETWORK) | `DPFLTR_IHVNETWORK_ID` | 80 | network |
/// | [`IHV_STREAMING`](Self::IHV_STREAMING) | `DPFLTR_IHVSTREAMING_ID` | 81 | kernel streaming |
/// | [`IHV_BUS`](Self::IHV_BUS) | `DPFLTR_IHVBUS_ID` | 82 | bus |
///
/// Each component has a filter mask of its own, named after it: `IHVNETWORK` in the
/// registry and `Kd_IHVNETWORK_Mask` in the debugger. [`DbgPrintLevel`] says how a record's
/// level is compared with it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct DbgPrintComponent(u32);

impl DbgPrintComponent {
    /// `DPFLTR_IHVDRIVER_ID` (77): any driver that is none of the types below.
    pub const IHV_DRIVER: Self = Self(77);
    /// `DPFLTR_IHVVIDEO_ID` (78): video drivers.
    pub const IHV_VIDEO: Self = Self(78);
    /// `DPFLTR_IHVAUDIO_ID` (79): audio drivers.
    pub const IHV_AUDIO: Self = Self(79);
    /// `DPFLTR_IHVNETWORK_ID` (80): network drivers.
    pub const IHV_NETWORK: Self = Self(80);
    /// `DPFLTR_IHVSTREAMING_ID` (81): kernel streaming drivers.
    pub const IHV_STREAMING: Self = Self(81);
    /// `DPFLTR_IHVBUS_ID` (82): bus drivers.
    pub const IHV_BUS: Self = Self(82);

    /// Any other component ID from `dpfilter.h`, for example one taken from wdk-sys.
    /// Windows reserves all but the six IHV IDs for its own components.
    #[must_use]
    pub const fn from_raw(component_id: u32) -> Self {
        Self(component_id)
    }

    /// The value passed to `DbgPrintEx`: also what ntpanic's `set_component_id` takes.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl fmt::Debug for DbgPrintComponent {
    /// Shows the component as the expression that makes it, such as
    /// `DbgPrintComponent::IHV_DRIVER`, so that its number need not be looked up.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match *self {
            Self::IHV_DRIVER => "IHV_DRIVER",
            Self::IHV_VIDEO => "IHV_VIDEO",
            Self::IHV_AUDIO => "IHV_AUDIO",
            Self::IHV_NETWORK => "IHV_NETWORK",
            Self::IHV_STREAMING => "IHV_STREAMING",
            Self::IHV_BUS => "IHV_BUS",
            Self(id) => return write!(f, "DbgPrintComponent::from_raw({id})"),
        };

        write!(f, "DbgPrintComponent::{name}")
    }
}

/// A `DbgPrintEx` level: the importance bits a record carries.
///
/// A record is shown if one of its importance bits is also set in the filter mask of its
/// [component](DbgPrintComponent). Windows ORs every component's mask with a system-wide
/// mask, `WIN2000`, which is `0x1` by default. So a record at [`ERROR`](Self::ERROR) is shown
/// whenever a kernel debugger is attached, and a record at any other level only once its
/// component's mask enables it, for example:
///
/// - in the debugger, with `ed nt!Kd_IHVDRIVER_Mask 0x8` for [`INFO`](Self::INFO): it takes
///   effect at once and lasts until Windows restarts;
/// - in the registry, as a DWORD value named after the component, such as `IHVDRIVER`, under
///   `HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Debug Print Filter`: it takes
///   effect at the next boot.
///
/// Microsoft keeps the ERROR bit for serious errors, since anyone who attaches a kernel
/// debugger sees them.
///
/// `DbgPrintEx` takes a level from 0 to 31 as a bit number and a larger value as the bits
/// themselves. [`bit`](Self::bit) and [`mask`](Self::mask) build either kind without that
/// ambiguity, and [`from_raw`](Self::from_raw) takes a value exactly as `DbgPrintEx` does.
///
/// This is not `log`'s level, which decides which records are logged at all.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct DbgPrintLevel(u32);

impl DbgPrintLevel {
    /// Bit 0, `DPFLTR_ERROR_LEVEL`: serious errors, shown whenever a kernel debugger is
    /// attached.
    pub const ERROR: Self = Self(0);
    /// Bit 1, `DPFLTR_WARNING_LEVEL`.
    pub const WARNING: Self = Self(1);
    /// Bit 2, `DPFLTR_TRACE_LEVEL`.
    pub const TRACE: Self = Self(2);
    /// Bit 3, `DPFLTR_INFO_LEVEL`.
    pub const INFO: Self = Self(3);

    /// The single importance bit `bit`, from 0 to 31, or `None` above 31.
    ///
    /// Bits 4 to 31 mean nothing to Windows, so a driver can take one of its own, to be
    /// shown apart from other drivers that print under the same component:
    ///
    /// ```
    /// use ntlogger::DbgPrintLevel;
    ///
    /// // In a constant, a bit above 31 is a compile error.
    /// const MY_DRIVER: DbgPrintLevel = DbgPrintLevel::bit(4).unwrap();
    ///
    /// // Shown once the component's mask has bit 4: `ed nt!Kd_IHVDRIVER_Mask 0x10`.
    /// assert_eq!(MY_DRIVER.importance_bits(), 0x10);
    /// ```
    #[must_use]
    pub const fn bit(bit: u32) -> Option<Self> {
        if bit < 32 { Some(Self(bit)) } else { None }
    }

    /// Several importance bits at once, or `None` if `mask` has none.
    ///
    /// Bit 31 is added, as `DPFLTR_MASK`, because `DbgPrintEx` takes a value below 32 as a
    /// bit number. A record at this level is therefore also shown when its component's
    /// mask has bit 31.
    ///
    /// ```
    /// use ntlogger::DbgPrintLevel;
    ///
    /// // Shown once the component's mask has INFO (bit 3) or bit 4.
    /// const INFO_OR_MINE: DbgPrintLevel = DbgPrintLevel::mask(0x18).unwrap();
    ///
    /// assert_eq!(INFO_OR_MINE.importance_bits(), 0x8000_0018);
    /// ```
    #[must_use]
    pub const fn mask(mask: u32) -> Option<Self> {
        if mask == 0 {
            None
        } else {
            Some(Self(mask | DPFLTR_MASK))
        }
    }

    /// A level exactly as `DbgPrintEx` takes it: from 0 to 31 a bit number, from 32 the bits
    /// themselves. For a value only known at run time, such as one read from the registry.
    #[must_use]
    pub const fn from_raw(level: u32) -> Self {
        Self(level)
    }

    /// The value passed to `DbgPrintEx`: also what ntpanic's `set_level` takes.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// The importance bits `DbgPrintEx` derives from this level: a record is shown if its
    /// component's filter mask shares one of them.
    #[must_use]
    pub const fn importance_bits(self) -> u32 {
        if self.0 < 32 { 1 << self.0 } else { self.0 }
    }
}

impl fmt::Debug for DbgPrintLevel {
    /// Shows the level as the expression that makes it: a name, [`bit`](Self::bit) for the
    /// other bit numbers, [`mask`](Self::mask) for bits with bit 31, and
    /// [`from_raw`](Self::from_raw) for the bits that only it makes. So `mask(0x18)` shows
    /// as `DbgPrintLevel::mask(0x18)`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::ERROR => f.write_str("DbgPrintLevel::ERROR"),
            Self::WARNING => f.write_str("DbgPrintLevel::WARNING"),
            Self::TRACE => f.write_str("DbgPrintLevel::TRACE"),
            Self::INFO => f.write_str("DbgPrintLevel::INFO"),
            Self(bit @ 4..32) => write!(f, "DbgPrintLevel::bit({bit})"),
            // mask(0) makes no level, but mask(DPFLTR_MASK) makes this one.
            Self(DPFLTR_MASK) => write!(f, "DbgPrintLevel::mask({DPFLTR_MASK:#X})"),
            Self(bits) if bits & DPFLTR_MASK != 0 => {
                write!(f, "DbgPrintLevel::mask({:#X})", bits & !DPFLTR_MASK)
            }
            Self(bits) => write!(f, "DbgPrintLevel::from_raw({bits:#X})"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::fmt::Write;

    /// `value` as `{:?}` shows it.
    fn debug_of(value: &impl fmt::Debug) -> heapless::String<64> {
        let mut shown = heapless::String::new();
        write!(shown, "{value:?}").unwrap();

        shown
    }

    #[test]
    fn a_component_shows_as_the_expression_that_makes_it() {
        let cases = [
            (
                DbgPrintComponent::IHV_DRIVER,
                "DbgPrintComponent::IHV_DRIVER",
            ),
            (DbgPrintComponent::IHV_VIDEO, "DbgPrintComponent::IHV_VIDEO"),
            (DbgPrintComponent::IHV_AUDIO, "DbgPrintComponent::IHV_AUDIO"),
            (
                DbgPrintComponent::IHV_NETWORK,
                "DbgPrintComponent::IHV_NETWORK",
            ),
            (
                DbgPrintComponent::IHV_STREAMING,
                "DbgPrintComponent::IHV_STREAMING",
            ),
            (DbgPrintComponent::IHV_BUS, "DbgPrintComponent::IHV_BUS"),
            // Either side of the named ones, and DPFLTR_SYSTEM_ID.
            (
                DbgPrintComponent::from_raw(76),
                "DbgPrintComponent::from_raw(76)",
            ),
            (
                DbgPrintComponent::from_raw(83),
                "DbgPrintComponent::from_raw(83)",
            ),
            (
                DbgPrintComponent::from_raw(0),
                "DbgPrintComponent::from_raw(0)",
            ),
        ];

        for (component, shown) in cases {
            assert_eq!(debug_of(&component), shown);
        }
    }

    #[test]
    fn a_level_shows_as_the_expression_that_makes_it() {
        let cases = [
            (DbgPrintLevel::ERROR, "DbgPrintLevel::ERROR"),
            (DbgPrintLevel::WARNING, "DbgPrintLevel::WARNING"),
            (DbgPrintLevel::TRACE, "DbgPrintLevel::TRACE"),
            (DbgPrintLevel::INFO, "DbgPrintLevel::INFO"),
            // A bit with a name shows as the name.
            (DbgPrintLevel::bit(3).unwrap(), "DbgPrintLevel::INFO"),
            (DbgPrintLevel::bit(4).unwrap(), "DbgPrintLevel::bit(4)"),
            (DbgPrintLevel::bit(31).unwrap(), "DbgPrintLevel::bit(31)"),
            (
                DbgPrintLevel::mask(0x18).unwrap(),
                "DbgPrintLevel::mask(0x18)",
            ),
            (
                DbgPrintLevel::mask(0x7FFF_FFFF).unwrap(),
                "DbgPrintLevel::mask(0x7FFFFFFF)",
            ),
            // Bit 31 alone, which mask(0) cannot make.
            (
                DbgPrintLevel::mask(0x8000_0000).unwrap(),
                "DbgPrintLevel::mask(0x80000000)",
            ),
            // Passed raw, 0x18 is bit number 24, and 0x8000_0018 is the mask above.
            (DbgPrintLevel::from_raw(0x18), "DbgPrintLevel::bit(24)"),
            (
                DbgPrintLevel::from_raw(0x8000_0018),
                "DbgPrintLevel::mask(0x18)",
            ),
            // Bits without bit 31, which only from_raw makes.
            (DbgPrintLevel::from_raw(32), "DbgPrintLevel::from_raw(0x20)"),
            (
                DbgPrintLevel::from_raw(0x7FFF_FFFF),
                "DbgPrintLevel::from_raw(0x7FFFFFFF)",
            ),
        ];

        for (level, shown) in cases {
            assert_eq!(debug_of(&level), shown, "{:#X}", level.raw());
        }
    }

    #[test]
    fn the_components_have_the_values_of_dpfilter_h() {
        let components = [
            DbgPrintComponent::IHV_DRIVER,
            DbgPrintComponent::IHV_VIDEO,
            DbgPrintComponent::IHV_AUDIO,
            DbgPrintComponent::IHV_NETWORK,
            DbgPrintComponent::IHV_STREAMING,
            DbgPrintComponent::IHV_BUS,
        ];
        assert_eq!(
            components.map(DbgPrintComponent::raw),
            [77, 78, 79, 80, 81, 82]
        );
    }

    #[test]
    fn the_named_levels_are_the_first_four_bits() {
        let levels = [
            DbgPrintLevel::ERROR,
            DbgPrintLevel::WARNING,
            DbgPrintLevel::TRACE,
            DbgPrintLevel::INFO,
        ];
        assert_eq!(levels.map(DbgPrintLevel::raw), [0, 1, 2, 3]);
        assert_eq!(
            levels.map(DbgPrintLevel::importance_bits),
            [0x1, 0x2, 0x4, 0x8]
        );
    }

    #[test]
    fn bit_takes_0_to_31_only() {
        assert_eq!(DbgPrintLevel::bit(0), Some(DbgPrintLevel::ERROR));
        assert_eq!(
            DbgPrintLevel::bit(31).map(DbgPrintLevel::importance_bits),
            Some(0x8000_0000)
        );
        assert_eq!(DbgPrintLevel::bit(32), None);
    }

    #[test]
    fn mask_keeps_its_bits_and_refuses_none() {
        // Passed raw, 0x18 is bit number 24.
        assert_eq!(DbgPrintLevel::from_raw(0x18).importance_bits(), 1 << 24);
        assert_eq!(
            DbgPrintLevel::mask(0x18).map(DbgPrintLevel::importance_bits),
            Some(0x8000_0018)
        );
        assert_eq!(DbgPrintLevel::mask(0), None);
    }

    #[test]
    fn raw_0_is_error_not_off() {
        assert_eq!(DbgPrintLevel::from_raw(0), DbgPrintLevel::ERROR);
    }
}
