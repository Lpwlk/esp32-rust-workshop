//! Firmware for node-B of the Ethernet app.
//!
//! Its settings are the preset `firmware/ethernet/presets/node-b.toml` (which extends `base.toml`, where every
//! setting is documented), turned into a `NodeConfig` by `build.rs`. Edit the TOML and rebuild; to
//! build another preset: `PRESET_NODE_B=node-b-tcp cargo run --release --bin node-b`.
#![no_std]
#![no_main]
#![deny(clippy::mem_forget)]

use esp32_rust_workshop_eth::config::NodeConfig;

const CONFIG: NodeConfig = include!(concat!(env!("OUT_DIR"), "/node_b.rs"));

esp32_rust_workshop_eth::node_firmware!(CONFIG);
