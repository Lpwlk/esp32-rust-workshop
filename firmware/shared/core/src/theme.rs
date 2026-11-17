//! Colors of the console output, chosen **at build time** (`[colors]` in a preset).
//!
//! The console never writes a color code itself: it names a **role** (the time stamp, the `TX` arrow, a
//! node name, an error...) and [`Paint`] writes the code that the active [`Theme`] gives that role.
//! A theme is part of the node's config ([`crate::config::NodeConfig::colors`]), so it is validated by the
//! compiler and costs nothing at run time to choose; it can't be changed from the CLI.
//!
//! ```toml
//! [colors]
//! tx    = "1;36"        # bold cyan
//! node  = "38;5;208"    # a 256-color orange
//! error = "1;37;41"     # bold white on red
//! ```
//!
//! A value is the list of SGR parameters of the ANSI escape code, as numbers separated by `;`
//! (`"31"` red, `"1;33"` bold yellow, `"38;5;208"` 256-color, `"38;2;255;128;0"` RGB), or `""` for no color.
//!
//! ```
//! use workshop_core::theme::{Role, Theme};
//!
//! let theme = Theme::DEFAULT.with(Role::Tx, "1;36");
//! assert_eq!(theme.get(Role::Tx).as_str(), "1;36");
//! assert_eq!(theme.get(Role::Rx).as_str(), Theme::DEFAULT.get(Role::Rx).as_str());
//! ```

use core::{
    fmt,
    sync::atomic::{AtomicPtr, Ordering},
};

/// Longest SGR parameter list: `38;2;255;128;0;48;2;255;128;0` is the longest one worth writing.
const MAX_SGR: usize = 32;

/// The parameters of one ANSI SGR escape code: `1;33` in `ESC[1;33m`. Empty means "no color".
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Sgr {
    len: u8,
    bytes: [u8; MAX_SGR],
}

impl Sgr {
    /// Parses `text`: digits separated by `;`, at most 32 characters, or empty.
    pub const fn parse(text: &str) -> Result<Sgr, &'static str> {
        let b = text.as_bytes();
        if b.len() > MAX_SGR {
            return Err("a color is at most 32 characters");
        }
        let mut bytes = [0u8; MAX_SGR];
        let mut i = 0;
        // No two `;` in a row, none at either end: `;` between numbers only.
        while i < b.len() {
            let c = b[i];
            let ok = c.is_ascii_digit() || (c == b';' && i > 0 && i + 1 < b.len() && b[i - 1] != b';');
            if !ok {
                return Err("a color is numbers separated by `;` (\"1;33\", \"38;5;208\"), or \"\"");
            }
            bytes[i] = c;
            i += 1;
        }
        Ok(Sgr { len: b.len() as u8, bytes })
    }

    /// [`Self::parse`] for a constant: a bad value fails the build.
    pub const fn new(text: &str) -> Sgr {
        match Self::parse(text) {
            Ok(sgr) => sgr,
            Err(_) => panic!("invalid color: numbers separated by `;`, at most 32 characters, or \"\""),
        }
    }

    /// The parameters, `1;33`.
    pub fn as_str(&self) -> &str {
        // Only ASCII digits and `;` are ever stored.
        core::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or("")
    }
}

impl fmt::Debug for Sgr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

macro_rules! roles {
    ($( $variant:ident $konst:ident, $key:literal, $default:literal, $what:literal; )*) => {
        /// A part of the console output that has its own color.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        #[allow(missing_docs)] // described in [`ROLES`]
        pub enum Role { $($variant),* }

        /// Every role: `(key in [colors], role, default, what it colors)`, in the order the docs list them.
        pub const ROLES: [(&str, Role, &str, &str); [$(Role::$variant),*].len()] =
            [$(($key, Role::$variant, $default, $what)),*];

        /// Number of roles.
        pub const ROLE_COUNT: usize = ROLES.len();

        impl Role {
            const fn index(self) -> usize { self as usize }
        }

        impl Theme {
            /// The theme the firmware has unless a preset says otherwise.
            pub const DEFAULT: Theme = Theme { codes: [$(Sgr::new($default)),*] };
        }

        #[allow(missing_docs)] // the constants named after the roles
        pub mod roles {
            use super::{Paint, Role};
            $(
                pub const $konst: Paint = Paint(Role::$variant);
            )*
        }
    };
}

roles! {
    Time TIME, "time", "90", "the time stamp at the start of a line";
    Dim DIM, "dim", "90", "labels and secondary text";
    Sep SEP, "sep", "90", "the `│` between the columns of a line";
    Tx TX, "tx", "36", "the `TX ▶` arrow, and what was sent";
    Rx RX, "rx", "32", "the `RX ◀` arrow, and what was received";
    Node NODE, "node", "33", "node names, and the peer's";
    Ok OK, "ok", "32", "good news: link up, checks passed";
    Warn WARN, "warn", "33", "things to look at: lost frames, control frames";
    Error ERROR, "error", "31", "errors, bad checksums, link down";
    Accent ACCENT, "accent", "36", "frames of the cards, names of settings and commands";
    Info INFO, "info", "34", "ordinary data frames in the trace";
    Prompt PROMPT, "prompt", "35", "the `cli ›` prompt and the echo of what you typed";
    Link LINK, "link", "35", "link events and the negotiated speed";
    Dat DAT, "dat", "30;47", "the `DAT` badge (periodic data)";
    Evt EVT, "evt", "30;43", "the `EVT` badge (button events)";
    Lnk LNK, "lnk", "30;45", "the `ECH`/`BAU` badge (link control)";
    Eth ETH, "eth", "34", "trace: the Ethernet layer";
    Ip IP, "ip", "35", "trace: the IPv4 and ARP layers";
    Transport TRANSPORT, "transport", "36", "trace: the UDP, TCP and ICMP layer";
    App APP, "app", "33", "trace: the node's own frames inside the packet";
    Raw RAW, "raw", "90", "trace: the hex dump";
    MixUdp MIX_UDP, "mix_udp", "30;44", "dashboard protocol ranking: the UDP bar (a background)";
    MixTcp MIX_TCP, "mix_tcp", "30;42", "dashboard protocol ranking: the TCP bar";
    MixArp MIX_ARP, "mix_arp", "30;43", "dashboard protocol ranking: the ARP bar";
    MixIcmp MIX_ICMP, "mix_icmp", "30;45", "dashboard protocol ranking: the ICMP bar";
    MixIp MIX_IP, "mix_ip", "30;46", "dashboard protocol ranking: other IPv4 protocols";
    MixOther MIX_OTHER, "mix_other", "30;100", "dashboard protocol ranking: everything else";
}

/// The colors of the console: one code per [`Role`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    codes: [Sgr; ROLE_COUNT],
}

impl Theme {
    /// This theme with `role` set to `codes` (a constant: a bad value fails the build).
    pub const fn with(mut self, role: Role, codes: &str) -> Theme {
        self.codes[role.index()] = Sgr::new(codes);
        self
    }

    /// The code of `role`.
    pub const fn get(&self, role: Role) -> &Sgr {
        &self.codes[role.index()]
    }

    /// Sets the role named `name` (`tx`, `node`...) from text, for the preset loader.
    pub fn set(&mut self, name: &str, value: &str) -> Result<(), &'static str> {
        let (_, role, _, _) = ROLES.iter().find(|(key, ..)| *key == name).ok_or("unknown color")?;
        self.codes[role.index()] = Sgr::parse(value.trim())?;
        Ok(())
    }

    /// The value of the role named `name`, as [`Self::set`] reads it back.
    pub fn value(&self, name: &str) -> Option<&str> {
        ROLES.iter().find(|(key, ..)| *key == name).map(|(_, role, ..)| self.codes[role.index()].as_str())
    }

    /// The Rust expression that builds this theme: the default, and one `.with()` per role that differs.
    pub fn write_rust(&self, out: &mut impl fmt::Write) -> fmt::Result {
        out.write_str("Theme::DEFAULT")?;
        for (_, role, default, _) in ROLES {
            if self.codes[role.index()].as_str() != default {
                write!(out, ".with(Role::{role:?}, {:?})", self.codes[role.index()].as_str())?;
            }
        }
        Ok(())
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::DEFAULT
    }
}

static ACTIVE: AtomicPtr<Theme> = AtomicPtr::new(core::ptr::null_mut());
static DEFAULT_THEME: Theme = Theme::DEFAULT;

/// Makes `theme` the one [`Paint`] uses. Called once at boot, with the theme of the node's build config.
pub fn activate(theme: &'static Theme) {
    ACTIVE.store(core::ptr::from_ref(theme).cast_mut(), Ordering::Release);
}

fn active() -> &'static Theme {
    let ptr = ACTIVE.load(Ordering::Acquire);
    // SAFETY: `activate` only stores a pointer to a `&'static Theme`, never written through.
    if ptr.is_null() { &DEFAULT_THEME } else { unsafe { &*ptr } }
}

/// Writes the escape code of a role when formatted: `{TIME}` in a format string turns the following
/// text to the time color; `{RST}` ends it.
#[derive(Clone, Copy)]
pub struct Paint(pub(crate) Role);

impl fmt::Display for Paint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sgr = active().get(self.0).as_str();
        if sgr.is_empty() { Ok(()) } else { write!(f, "\x1b[{sgr}m") }
    }
}

/// Ends any style.
pub const RST: &str = "\x1b[0m";
/// Bold on.
pub const BOLD: &str = "\x1b[1m";
/// Faint on.
pub const FAINT: &str = "\x1b[2m";

#[cfg(test)]
mod tests {
    use std::string::{String, ToString};

    use super::*;

    #[test]
    fn sgr_accepts_numbers_and_semicolons_only() {
        for good in ["", "0", "1;33", "38;5;208", "38;2;255;128;0", "1;37;41"] {
            assert_eq!(Sgr::parse(good).unwrap().as_str(), good);
        }
        for bad in [";", "1;", ";1", "1;;2", "red", "1 2", "\x1b[31m", "1;3m", "-1", "1,2"] {
            assert!(Sgr::parse(bad).is_err(), "{bad:?}");
        }
        assert!(Sgr::parse(&"1;".repeat(20)).is_err(), "too long");
        // The longest worth writing fits.
        assert!(Sgr::parse("38;2;255;128;0;48;2;255;128;0").is_ok());
    }

    #[test]
    fn default_theme_is_the_original_look() {
        let t = Theme::DEFAULT;
        assert_eq!(t.get(Role::Tx).as_str(), "36");
        assert_eq!(t.get(Role::Rx).as_str(), "32");
        assert_eq!(t.get(Role::Dat).as_str(), "30;47");
    }

    #[test]
    fn role_table_is_consistent() {
        for (i, (key, role, default, what)) in ROLES.iter().enumerate() {
            assert_eq!(role.index(), i, "{key}: roles are listed in declaration order");
            assert!(Sgr::parse(default).is_ok(), "{key}");
            assert!(!what.is_empty());
            assert!(ROLES[i + 1..].iter().all(|(k, ..)| k != key), "{key} twice");
        }
    }

    #[test]
    fn set_and_value_round_trip() {
        let mut t = Theme::DEFAULT;
        t.set("tx", "1;36").unwrap();
        assert_eq!(t.value("tx"), Some("1;36"));
        assert!(t.set("tx", "cyan").is_err());
        assert!(t.set("nope", "1").is_err());
        assert_eq!(t.value("tx"), Some("1;36"), "a refused value changes nothing");
        assert_eq!(t.value("nope"), None);
        t.set("tx", "").unwrap();
        assert_eq!(t.value("tx"), Some(""));
    }

    #[test]
    fn write_rust_lists_only_the_differences() {
        let mut out = String::new();
        Theme::DEFAULT.write_rust(&mut out).unwrap();
        assert_eq!(out, "Theme::DEFAULT");
        let t = Theme::DEFAULT.with(Role::Node, "38;5;208").with(Role::Error, "1;31");
        out.clear();
        t.write_rust(&mut out).unwrap();
        assert_eq!(out, "Theme::DEFAULT.with(Role::Node, \"38;5;208\").with(Role::Error, \"1;31\")");
    }

    #[test]
    fn paint_follows_the_active_theme() {
        static CUSTOM: Theme = Theme::DEFAULT.with(Role::Tx, "1;35").with(Role::Rx, "");
        // Before `activate`: the default.
        assert_eq!(roles::TX.to_string(), "\x1b[36m");
        activate(&CUSTOM);
        assert_eq!(roles::TX.to_string(), "\x1b[1;35m");
        assert_eq!(roles::RX.to_string(), "", "no code, no escape");
        assert_eq!(roles::NODE.to_string(), "\x1b[33m");
        // Put the default back for the other tests of this process.
        static DEFAULT: Theme = Theme::DEFAULT;
        activate(&DEFAULT);
    }
}
