//! Firmware for node-A.
//!
//! Its settings are the preset `presets/uart/node-a.toml` (which extends `base.toml`, where
//! every setting is documented), turned into a `NodeConfig` by `build.rs`. Edit the TOML and
//! rebuild; to build another preset: `PRESET_NODE_A=my-preset cargo run --release --bin node-a`.
//! Settings that must match on both nodes, like the baud rate, are in `config::shared`.
#![no_std]
#![no_main]
#![deny(clippy::mem_forget)]

use esp32_rust_workshop::config::NodeConfig;

const CONFIG: NodeConfig = include!(concat!(env!("OUT_DIR"), "/node_a.rs"));

esp32_rust_workshop::node_firmware!(CONFIG);
