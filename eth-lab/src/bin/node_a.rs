//! Firmware for node-A of the Ethernet app.
//!
//! Its settings are the preset `presets/eth/node-a.toml` (which extends `base.toml`, where every
//! setting is documented), turned into a `NodeConfig` by `build.rs`. Edit the TOML and rebuild; to
//! build another preset: `PRESET_NODE_A=node-a-tcp cargo run --release --bin node-a`.
#![no_std]
#![no_main]
#![deny(clippy::mem_forget)]

use eth_lab::config::NodeConfig;

const CONFIG: NodeConfig = include!(concat!(env!("OUT_DIR"), "/node_a.rs"));

eth_lab::node_firmware!(CONFIG);
