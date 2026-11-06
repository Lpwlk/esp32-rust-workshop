//! Settings saved in flash (CLI `save` / `forget`), with the `persist` feature.
//!
//! The record format (only the settings that differ from the build, CRC-checked)
//! is [`workshop_core::persist`], tested on the host. This module only finds where
//! to keep it and moves bytes:
//!
//! - **where**: the first 4 KB sector of the `nvs` data partition of the default
//!   partition table (0x9000). This firmware doesn't use ESP-IDF's NVS, so the area
//!   is free; `espflash flash` doesn't write it, so saved settings survive a
//!   reflash (`espflash erase-flash` / `dev.py erase` wipes them);
//! - **how**: esp-storage's read-modify-erase-write of that one sector (about
//!   50 ms, done only on `save` / `forget`).
//!
//! Without the feature, [`Store`] still exists but has no storage: `save` says so.

use workshop_core::{config::NodeConfig, persist::Loaded};

/// What happened to the saved settings at boot, for the boot card.
#[derive(Clone, Copy, Debug)]
pub enum BootLoad {
    /// Built without the `persist` feature.
    Disabled,
    /// No `nvs` partition in the partition table, or flash not readable.
    NoStorage,
    /// Nothing saved.
    Empty,
    /// Saved settings applied.
    Applied(Loaded),
    /// A damaged record (ignored).
    Corrupt,
    /// Saved by another node's firmware (ignored).
    OtherNode,
}

/// Why `save` or `forget` failed.
#[derive(Clone, Copy, Debug)]
pub enum StoreError {
    /// Built without the `persist` feature.
    Disabled,
    /// No `nvs` partition found at boot.
    NoStorage,
    /// The flash driver refused the write.
    Flash,
}

impl core::fmt::Display for StoreError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            StoreError::Disabled => "this firmware is built without the `persist` feature",
            StoreError::NoStorage => "no `nvs` partition in the partition table",
            StoreError::Flash => "flash write failed",
        })
    }
}

#[cfg(feature = "persist")]
mod imp {
    use esp_bootloader_esp_idf::partitions::{self, DataPartitionSubType, PartitionType};
    use esp_hal::peripherals::FLASH;
    use esp_storage::FlashStorage;
    use workshop_core::persist::{self, LoadError, MAX_RECORD};

    use super::*;

    pub struct Store {
        flash: FlashStorage<'static>,
        /// Flash offset of the record, if an `nvs` partition was found.
        offset: Option<u32>,
    }

    impl Store {
        pub fn new(flash: FLASH<'static>) -> Self {
            let mut flash = FlashStorage::new(flash);
            let mut table = [0u8; partitions::PARTITION_TABLE_MAX_LEN];
            let offset = partitions::read_partition_table(&mut flash, &mut table)
                .ok()
                .and_then(|t| t.find_partition(PartitionType::Data(DataPartitionSubType::Nvs)).ok().flatten())
                .filter(|p| p.len() >= FlashStorage::SECTOR_SIZE)
                .map(|p| p.offset());
            Self { flash, offset }
        }

        /// Applies the saved settings to `cfg` (the build config).
        pub fn load(&mut self, cfg: &mut NodeConfig) -> BootLoad {
            let Some(offset) = self.offset else { return BootLoad::NoStorage };
            let mut buf = [0u8; MAX_RECORD];
            if self.flash.read(offset, &mut buf).is_err() {
                return BootLoad::NoStorage;
            }
            match persist::decode(&buf, cfg) {
                Ok(loaded) => BootLoad::Applied(loaded),
                Err(LoadError::Empty) => BootLoad::Empty,
                Err(LoadError::Corrupt) => BootLoad::Corrupt,
                Err(LoadError::OtherNode(_)) => BootLoad::OtherNode,
            }
        }

        /// Saves the settings of `live` that differ from `build`; returns how many.
        pub fn save(&mut self, live: &NodeConfig, build: &NodeConfig) -> Result<usize, StoreError> {
            let offset = self.offset.ok_or(StoreError::NoStorage)?;
            let record = persist::encode(live, build);
            self.flash.write(offset, &record).map_err(|_| StoreError::Flash)?;
            Ok(persist::changed(live, build))
        }

        /// Erases the saved settings (the record's magic becomes 0xFF: "empty").
        pub fn forget(&mut self) -> Result<(), StoreError> {
            let offset = self.offset.ok_or(StoreError::NoStorage)?;
            self.flash.write(offset, &[0xFF; persist::HEADER_LEN]).map_err(|_| StoreError::Flash)
        }
    }
}

#[cfg(not(feature = "persist"))]
mod imp {
    use super::*;

    pub struct Store;

    impl Store {
        pub fn new(_flash: esp_hal::peripherals::FLASH<'static>) -> Self {
            Self
        }

        pub fn load(&mut self, _cfg: &mut NodeConfig) -> BootLoad {
            BootLoad::Disabled
        }

        pub fn save(&mut self, _live: &NodeConfig, _build: &NodeConfig) -> Result<usize, StoreError> {
            Err(StoreError::Disabled)
        }

        pub fn forget(&mut self) -> Result<(), StoreError> {
            Err(StoreError::Disabled)
        }
    }
}

/// The saved-settings area in flash.
pub use imp::Store;
