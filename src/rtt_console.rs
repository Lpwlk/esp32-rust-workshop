//! RTT console, with the `rtt` Cargo feature (commands: docs/book/guide-rust.md, "Console RTT").
//!
//! RTT (Real-Time Transfer) goes through the debug probe (here the board's own
//! USB Serial/JTAG, via probe-rs): the firmware writes into RAM buffers, the probe
//! reads them in the background. Two channels:
//!
//! - up 0 `console`: the console output, same text as on USB (with its colors:
//!   `probe-rs attach` shows them, cargo-embed strips them). The `console` setting
//!   chooses where output goes: `auto`/`both` (USB + RTT), `serial`, `rtt`;
//! - down 0 `cli`: commands typed in cargo-embed's input field, fed to the same CLI.
//!
//! Without the feature, [`init`] and [`read`] do nothing and RTT costs nothing.

#[cfg(feature = "rtt")]
mod imp {
    use core::cell::RefCell;

    use critical_section::Mutex;
    use rtt_target::{ChannelMode, DownChannel, rtt_init, set_print_channel};

    static DOWN: Mutex<RefCell<Option<DownChannel>>> = Mutex::new(RefCell::new(None));

    pub fn init() {
        let channels = rtt_init! {
            up: {
                // NoBlockTrim never blocks: without a probe attached, output that
                // doesn't fit is cut instead of freezing the firmware.
                0: { size: 4096, mode: ChannelMode::NoBlockTrim, name: "console" }
            }
            down: {
                0: { size: 128, name: "cli" }
            }
        };
        set_print_channel(channels.up.0);
        critical_section::with(|cs| DOWN.borrow_ref_mut(cs).replace(channels.down.0));
    }

    pub fn read(buf: &mut [u8]) -> usize {
        critical_section::with(|cs| DOWN.borrow_ref_mut(cs).as_mut().map_or(0, |d| d.read(buf)))
    }
}

#[cfg(not(feature = "rtt"))]
mod imp {
    pub fn init() {}

    pub fn read(_buf: &mut [u8]) -> usize {
        0
    }
}

/// Sets up the RTT channels; call once, before printing anything.
pub use imp::init;
/// Bytes typed in the probe's terminal (down channel 0), if any.
pub use imp::read;

use core::sync::atomic::{AtomicBool, Ordering};

use workshop_core::config::Console;

/// Whether the `rtt` feature is built in.
pub const BUILT: bool = cfg!(feature = "rtt");

static SERIAL_ON: AtomicBool = AtomicBool::new(true);
static RTT_ON: AtomicBool = AtomicBool::new(BUILT);

/// Routes the console output (setting `console`). Without the `rtt` feature,
/// everything stays on the USB serial console, whatever the setting.
pub fn set_route(console: Console) {
    SERIAL_ON.store(console.serial(BUILT), Ordering::Relaxed);
    RTT_ON.store(console.rtt(BUILT), Ordering::Relaxed);
}

/// Output goes to the USB serial console.
#[doc(hidden)]
pub fn serial_on() -> bool {
    SERIAL_ON.load(Ordering::Relaxed)
}

/// Output goes to RTT.
#[doc(hidden)]
pub fn rtt_on() -> bool {
    RTT_ON.load(Ordering::Relaxed)
}

/// Prints to the console(s) chosen by the `console` setting.
#[macro_export]
#[doc(hidden)]
macro_rules! con_print {
    ($($arg:tt)*) => {{
        if $crate::rtt_console::serial_on() {
            esp_println::print!($($arg)*);
        }
        // `format_args!` built here keeps inline captures (`{CYAN}`) working.
        #[cfg(feature = "rtt")]
        if $crate::rtt_console::rtt_on() {
            rtt_target::rprint!("{}", format_args!($($arg)*));
        }
    }};
}

/// [`con_print!`] with a newline.
#[macro_export]
#[doc(hidden)]
macro_rules! con_println {
    () => {
        $crate::con_print!("\n")
    };
    ($($arg:tt)*) => {
        $crate::con_print!("{}\n", format_args!($($arg)*))
    };
}
