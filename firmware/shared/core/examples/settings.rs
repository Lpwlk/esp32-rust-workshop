//! Node settings: a compile-time config, changes as the CLI makes them, and the
//! record `save` writes to flash.
//!
//! cargo run -p workshop-core --example settings --target host-tuple

use workshop_core::config::{Key, LedMode, NodeConfig, SetError};
use workshop_core::persist;

// --8<-- [start:const]
/// What a node file (src/bin/node_a.rs) declares: only the differences.
const BUILD: NodeConfig = NodeConfig {
    name: "node-A",
    data_period_ms: 1000.0,
    led1: LedMode::Blink { period_ms: 250.0 },
    ..NodeConfig::DEFAULT
};

// The same check `node_firmware!` adds: a bad value here is a *build* error.
const _: () = BUILD.check();
// --8<-- [end:const]

fn main() {
    // --8<-- [start:set]
    let mut live = BUILD; // the firmware works on a copy

    // `set <key> <value>` in the CLI calls exactly this:
    live.set("rgb.pattern", "ocean").unwrap();
    live.set("data_period_ms", "4Hz").unwrap(); // a frequency is a period too
    assert_eq!(live.data_period_ms, 250.0);

    // Invalid values are refused, and leave the config untouched.
    assert!(matches!(live.set("rgb.brightness", "150"), Err(SetError::Invalid(_))));
    assert!(matches!(live.set("led1", "sometimes"), Err(SetError::BadValue(_))));
    assert_eq!(live.set("pins.button", "5"), Err(SetError::ReadOnly));

    println!("rgb.pattern    = {}", live.value(Key::RgbPattern));
    println!("data_period_ms = {}", live.value(Key::DataPeriod));
    // --8<-- [end:set]

    // --8<-- [start:save]
    // `save`: only what differs from the build, as text, CRC-protected.
    let record = persist::encode(&live, &BUILD);
    println!("record: {} bytes, {} settings changed", record.len(), persist::changed(&live, &BUILD));

    // Next boot: build config, then the record on top, each value re-validated.
    let mut booted = BUILD;
    let loaded = persist::decode(&record, &mut booted).unwrap();
    assert_eq!(booted, live);
    println!("boot: {} applied, {} skipped", loaded.applied, loaded.skipped);
    // --8<-- [end:save]
}
