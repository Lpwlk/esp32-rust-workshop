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
//! - **how**: esp-hal's own flash driver: erase that one sector and write the record (about
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
    use esp_hal::{
        Blocking,
        flash::{Config, Flash},
        peripherals::FLASH,
    };
    use workshop_core::persist::{self, LoadError, MAX_RECORD};

    use super::*;

    pub struct Store {
        /// `None` if the flash chip isn't recognised.
        flash: Option<Flash<'static, Blocking>>,
        /// Flash offset of the record, if an `nvs` partition was found.
        offset: Option<u32>,
    }

    impl Store {
        pub fn new(flash: FLASH<'static>) -> Self {
            let Ok(mut flash) = Flash::new(flash, Config::default()) else { return Self { flash: None, offset: None } };
            let mut table = [0u8; partitions::PARTITION_TABLE_MAX_LEN];
            let offset = partitions::read_partition_table(&mut flash, &mut table)
                .ok()
                .and_then(|t| t.find_partition(PartitionType::Data(DataPartitionSubType::Nvs)).ok().flatten())
                .filter(|p| p.len() >= Flash::SECTOR_SIZE)
                .map(|p| p.offset());
            Self { flash: Some(flash), offset }
        }

        /// Applies the saved settings to `cfg` (the build config).
        pub fn load(&mut self, cfg: &mut NodeConfig) -> BootLoad {
            let (Some(flash), Some(offset)) = (self.flash.as_mut(), self.offset) else { return BootLoad::NoStorage };
            let mut words = [0u32; MAX_RECORD / 4];
            if flash.read(offset, &mut words).is_err() {
                return BootLoad::NoStorage;
            }
            let mut buf = [0u8; MAX_RECORD];
            for (bytes, word) in buf.as_chunks_mut::<4>().0.iter_mut().zip(words) {
                *bytes = word.to_le_bytes();
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
            let record = persist::encode(live, build);
            // Whole 32-bit words, padding with the erased value.
            let mut words = [u32::MAX; MAX_RECORD / 4];
            for (word, chunk) in words.iter_mut().zip(record.chunks(4)) {
                let mut bytes = [0xFF; 4];
                bytes[..chunk.len()].copy_from_slice(chunk);
                *word = u32::from_le_bytes(bytes);
            }
            let (flash, offset) = self.area()?;
            // SAFETY: the `nvs` partition isn't mapped for code or data: nothing runs from it.
            unsafe {
                flash.erase(offset, offset + Flash::SECTOR_SIZE).map_err(|_| StoreError::Flash)?;
                flash.write(offset, &words[..record.len().div_ceil(4)]).map_err(|_| StoreError::Flash)?;
            }
            Ok(persist::changed(live, build))
        }

        /// Erases the saved settings (the sector goes back to 0xFF: "empty").
        pub fn forget(&mut self) -> Result<(), StoreError> {
            let (flash, offset) = self.area()?;
            // SAFETY: as in `save`.
            unsafe { flash.erase(offset, offset + Flash::SECTOR_SIZE).map_err(|_| StoreError::Flash) }
        }

        fn area(&mut self) -> Result<(&mut Flash<'static, Blocking>, u32), StoreError> {
            match (self.flash.as_mut(), self.offset) {
                (Some(flash), Some(offset)) => Ok((flash, offset)),
                _ => Err(StoreError::NoStorage),
            }
        }
    }
}

// --8<-- [start:stub]
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
// --8<-- [end:stub]
