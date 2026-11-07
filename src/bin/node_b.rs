//! Firmware for node-B.
//!
//! Per-node settings: override any field of `NodeConfig::DEFAULT` below
//! (see `crates/workshop-core/src/config.rs`, or the settings reference). Settings that
//! must match on both nodes, like the baud rate, are in `config::shared` instead.
#![no_std]
#![no_main]
#![deny(clippy::mem_forget)]

use esp32_rust_workshop::config::{LedMode, NodeConfig};

const CONFIG: NodeConfig = NodeConfig {
    name: "node-B",
    // Examples of per-node overrides (uncomment / edit):
    // data_period_ms: 1000.0,
    // status_period_ms: 5000.0,
    // rgb: RgbConfig { pattern: "ocean", ..NodeConfig::DEFAULT.rgb },
    // pins: Pins { led1: 22, ..NodeConfig::DEFAULT.pins },
    led1: LedMode::PeerButton,
    led2: LedMode::RxFlash,
    ..NodeConfig::DEFAULT
};

esp32_rust_workshop::node_firmware!(CONFIG);
