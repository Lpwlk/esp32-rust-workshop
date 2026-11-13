//! Settings saved in flash (CLI `save`), and read back at boot.
//!
//! A record holds **only the settings that differ from the build config**, as the
//! text the CLI `set` accepts (`rgb.pattern=ocean`). So:
//!
//! - a new firmware with new defaults still applies them to every setting you
//!   didn't save;
//! - a setting renamed or removed by a new firmware is skipped, not misread;
//! - every saved value goes through [`NodeConfig::set`], hence through the same
//!   validation as the CLI: a stored value can't put the node in a state the
//!   build would refuse.
//!
//! Layout (little-endian):
//!
//! ```text
//! offset 0   "WSC1"        magic + format version
//!        4   u16           payload length
//!        6   u32           CRC-32 (IEEE) of the payload
//!       10   payload       "node=node-A\nrgb.pattern=ocean\nverbose=on\n"
//! ```
//!
//! The `node=` line ties the record to a node: node-B firmware flashed on a board
//! that saved node-A's settings ignores them.
//!
//! ```
//! use workshop_core::config::NodeConfig;
//! use workshop_core::persist;
//!
//! let build = NodeConfig { name: "node-A", ..NodeConfig::DEFAULT };
//! let mut live = build;
//! live.set("rgb.pattern", "ocean").unwrap();
//!
//! let record = persist::encode(&live, &build);
//! let mut booted = build;
//! let loaded = persist::decode(&record, &mut booted).unwrap();
//! assert_eq!((loaded.applied, loaded.skipped), (1, 0));
//! assert_eq!(booted, live);
//! ```

use core::fmt::Write as _;

use crate::config::{KEYS, NodeConfig};

/// Magic and format version at the start of a record.
pub const MAGIC: [u8; 4] = *b"WSC1";
/// Bytes before the payload: magic, length, CRC.
pub const HEADER_LEN: usize = 10;
/// Largest record, header included. Every setting changed takes about 600 bytes.
pub const MAX_RECORD: usize = 1024;

/// A record ready to be written to flash.
pub type Record = heapless::Vec<u8, MAX_RECORD>;

/// CRC-32 (IEEE 802.3, the one of zip and Ethernet), bitwise: small, and fast
/// enough for one record at boot.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// Settings of `live` that differ from `build`, as a record.
pub fn encode(live: &NodeConfig, build: &NodeConfig) -> Record {
    let mut payload: heapless::String<{ MAX_RECORD - HEADER_LEN }> = heapless::String::new();
    let _ = writeln!(payload, "node={}", build.name);
    for (name, key) in KEYS {
        let value = live.value(key);
        if value != build.value(key) {
            // Can't overflow: 16 keys × (20 + 24 + 2) bytes < 1014.
            let _ = writeln!(payload, "{name}={value}");
        }
    }
    let mut record = Record::new();
    let _ = record.extend_from_slice(&MAGIC);
    let _ = record.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    let _ = record.extend_from_slice(&crc32(payload.as_bytes()).to_le_bytes());
    let _ = record.extend_from_slice(payload.as_bytes());
    record
}

/// Number of settings that differ from `build` (what `save` would store).
pub fn changed(live: &NodeConfig, build: &NodeConfig) -> usize {
    KEYS.iter().filter(|(_, k)| live.value(*k) != build.value(*k)).count()
}

/// Why no setting was read back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadError<'a> {
    /// Erased flash: nothing was ever saved, or it was forgotten.
    Empty,
    /// Not a record of this format, wrong length or wrong CRC.
    Corrupt,
    /// A record saved by another node (name given).
    OtherNode(&'a str),
}

/// What [`decode`] applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Loaded {
    /// Settings applied.
    pub applied: usize,
    /// Lines skipped: unknown or read-only key, or a value this firmware refuses.
    pub skipped: usize,
}

/// Applies a record (read from flash, possibly longer than the record) to `cfg`,
/// which should hold the build config.
pub fn decode<'a>(bytes: &'a [u8], cfg: &mut NodeConfig) -> Result<Loaded, LoadError<'a>> {
    if bytes.len() >= MAGIC.len() && bytes[..MAGIC.len()].iter().all(|&b| b == 0xFF) {
        return Err(LoadError::Empty);
    }
    if bytes.len() < HEADER_LEN || bytes[..4] != MAGIC {
        return Err(LoadError::Corrupt);
    }
    let len = usize::from(u16::from_le_bytes([bytes[4], bytes[5]]));
    let crc = u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]);
    let payload = bytes.get(HEADER_LEN..HEADER_LEN + len).ok_or(LoadError::Corrupt)?;
    if crc32(payload) != crc {
        return Err(LoadError::Corrupt);
    }
    let text = core::str::from_utf8(payload).map_err(|_| LoadError::Corrupt)?;

    let mut lines = text.lines();
    match lines.next().and_then(|l| l.strip_prefix("node=")) {
        Some(name) if name == cfg.name => {}
        Some(name) => return Err(LoadError::OtherNode(name)),
        None => return Err(LoadError::Corrupt),
    }
    let mut loaded = Loaded { applied: 0, skipped: 0 };
    for line in lines {
        match line.split_once('=') {
            Some((key, value)) if cfg.set(key, value).is_ok() => loaded.applied += 1,
            _ => loaded.skipped += 1,
        }
    }
    Ok(loaded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Console, LedMode, TxMode};

    const BUILD: NodeConfig = NodeConfig { name: "node-A", ..NodeConfig::DEFAULT };

    fn tweaked() -> NodeConfig {
        let mut c = BUILD;
        for (k, v) in [
            ("data_period_ms", "400"),
            ("rgb.pattern", "ocean"),
            ("rgb.freq_hz", "37.5"),
            ("led2", "blink 2.5Hz"),
            ("tx_mode", "on-rx"),
            ("verbose", "on"),
            ("console", "both"),
            ("burst.rate_hz", "1.5kHz"),
        ] {
            c.set(k, v).unwrap_or_else(|e| panic!("{k}: {e:?}"));
        }
        c
    }

    #[test]
    fn crc_known_value() {
        // The standard check value of CRC-32/IEEE.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn round_trip_restores_exactly() {
        let live = tweaked();
        let record = encode(&live, &BUILD);
        let mut booted = BUILD;
        assert_eq!(decode(&record, &mut booted), Ok(Loaded { applied: 8, skipped: 0 }));
        assert_eq!(booted, live);
        assert_eq!(
            (booted.tx_mode, booted.console, booted.led2),
            (TxMode::OnRx, Console::Both, LedMode::Blink { period_ms: 400.0 })
        );
    }

    #[test]
    fn only_changes_are_stored() {
        let record = encode(&BUILD, &BUILD);
        assert_eq!(&record[HEADER_LEN..], b"node=node-A\n");
        assert_eq!(changed(&tweaked(), &BUILD), 8);
        assert_eq!(changed(&BUILD, &BUILD), 0);
    }

    #[test]
    fn every_setting_changed_still_fits() {
        // The longest value of each key.
        let mut c = BUILD;
        for (k, v) in [
            ("data_period_ms", "3333.333"),
            ("status_period_ms", "3333.333"),
            ("link_timeout_periods", "99"),
            ("debounce_ms", "999"),
            ("lockout_ms", "4999"),
            ("rgb.pattern", "sakura"),
            ("rgb.freq_hz", "199.99"),
            ("rgb.cycle_ms", "59999.99"),
            ("rgb.brightness", "99"),
            ("led1", "blink 59999.99"),
            ("led2", "blink 12345.678"),
            ("verbose", "on"),
            ("tx_mode", "quiet"),
            ("burst.count", "99999"),
            ("burst.rate_hz", "19999.99"),
            ("console", "serial"),
        ] {
            c.set(k, v).unwrap_or_else(|e| panic!("{k}: {e:?}"));
        }
        assert_eq!(changed(&c, &BUILD), KEYS.len(), "the test must change every key");
        let record = encode(&c, &BUILD);
        assert!(record.len() < MAX_RECORD / 2, "{} bytes", record.len());
        let mut booted = BUILD;
        assert_eq!(decode(&record, &mut booted).map(|l| l.applied), Ok(KEYS.len()));
        assert_eq!(booted, c);
    }

    #[test]
    fn erased_and_damaged_flash() {
        let mut cfg = BUILD;
        assert_eq!(decode(&[0xFF; 64], &mut cfg), Err(LoadError::Empty));
        assert_eq!(decode(&[0x00; 64], &mut cfg), Err(LoadError::Corrupt));
        assert_eq!(decode(b"WSC", &mut cfg), Err(LoadError::Corrupt));

        let good = encode(&tweaked(), &BUILD);
        // Any flipped bit in the payload or the CRC is caught.
        for i in 6..good.len() {
            let mut bad = good.clone();
            bad[i] ^= 0x04;
            assert_eq!(decode(&bad, &mut cfg), Err(LoadError::Corrupt), "byte {i}");
        }
        // A length pointing past the end.
        let mut bad = good.clone();
        bad[4] = 0xFF;
        assert_eq!(decode(&bad, &mut cfg), Err(LoadError::Corrupt));
        assert_eq!(cfg, BUILD, "nothing applied from a damaged record");
    }

    #[test]
    fn record_of_another_node_is_ignored() {
        let record = encode(&tweaked(), &BUILD);
        let mut other = NodeConfig { name: "node-B", ..NodeConfig::DEFAULT };
        assert_eq!(decode(&record, &mut other), Err(LoadError::OtherNode("node-A")));
        assert_eq!(other, NodeConfig { name: "node-B", ..NodeConfig::DEFAULT });
    }

    #[test]
    fn unknown_or_refused_lines_are_skipped() {
        // As written by a future firmware: a new key, a value this one refuses,
        // a read-only key, and a valid line.
        let payload = b"node=node-A\nwifi.ssid=lab\nrgb.pattern=does-not-exist\nname=x\nverbose=on\n";
        let mut record = Record::new();
        record.extend_from_slice(&MAGIC).unwrap();
        record.extend_from_slice(&(payload.len() as u16).to_le_bytes()).unwrap();
        record.extend_from_slice(&crc32(payload).to_le_bytes()).unwrap();
        record.extend_from_slice(payload).unwrap();

        let mut cfg = BUILD;
        assert_eq!(decode(&record, &mut cfg), Ok(Loaded { applied: 1, skipped: 3 }));
        assert_eq!(cfg.verbose, crate::config::Verbosity::Wire);
        assert_eq!(cfg.name, "node-A");
    }

    #[test]
    fn trailing_flash_bytes_are_ignored() {
        let mut sector = [0xFF; 256];
        let record = encode(&tweaked(), &BUILD);
        sector[..record.len()].copy_from_slice(&record);
        let mut cfg = BUILD;
        assert_eq!(decode(&sector, &mut cfg).map(|l| l.applied), Ok(8));
    }
}
