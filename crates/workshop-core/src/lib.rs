//! Hardware-independent logic of the UART workshop firmware.
//!
//! Everything in this crate is plain `no_std` Rust that never touches a
//! peripheral, so it builds:
//!
//! - for the **ESP32-S31** (`riscv32imafc-unknown-none-elf`): the firmware crate
//!   `esp32-rust-workshop` depends on it and re-exports its modules;
//! - for the **host**: `cargo test-host` runs its unit tests, property tests and
//!   the examples of this documentation.
//!
//! Rule of thumb when adding code: if a piece of logic can be written without a
//! peripheral, put it here and test it on the host; keep only the hardware glue
//! (GPIO, UART, RMT, interrupts) in the firmware crate, and test that on the board
//! (`cargo test-target`).
//!
//! | Module | What it holds |
//! |---|---|
//! | [`protocol`] | frame format on the UART link: encode / parse |
//! | [`line`](mod@line) | splitting the received byte stream into lines |
//! | [`seq`] | lost-frame and peer-restart detection |
//! | [`config`] | node configuration, its validation and the CLI `set` |
//! | [`cli`] | CLI command parser and help table |
//! | [`editor`] | CLI line editor: cursor, history |
//! | [`button`] | jumper-button debouncing state machine |
//! | [`leds`] | what a breadboard LED shows in each mode |
//! | [`fx`] | RGB LED color patterns and fade math |
//! | [`baud`] | link speed change handshake (CLI `baud`) |
//! | [`rtt`] | round-trip time from `ECH` replies (`tx_mode on-rx`) |
//! | [`burst`] | burst scheduling and statistics (CLI `burst`) |
//! | [`units`] | periods and frequencies typed in the CLI (decimals, units) |
//! | [`wire`] | when frames were on the wire; shared TX/RX delta between `DAT` frames |
//! | [`persist`] | settings saved in flash (CLI `save`): record format, CRC, read back at boot |
//! | [`docgen`] | Markdown reference pages generated from the tables above |
#![cfg_attr(not(test), no_std)]
#![warn(missing_docs)]

pub mod baud;
pub mod burst;
pub mod button;
pub mod cli;
pub mod config;
pub mod docgen;
pub mod editor;
pub mod fx;
pub mod leds;
pub mod line;
pub mod persist;
pub mod protocol;
pub mod rtt;
pub mod seq;
#[doc(hidden)]
pub mod test_vectors;
pub mod units;
pub mod wire;
