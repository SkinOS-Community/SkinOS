//! Steam download cells and their Valve-provided display names.
//!
//! The bundled catalog is public Steam client metadata, not account data.
//! See resources/download-regions.md for the source and refresh process.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DownloadRegion {
    pub id: u32,
    pub name: String,
}

pub fn download_regions() -> &'static [DownloadRegion] {
    static REGIONS: OnceLock<Vec<DownloadRegion>> = OnceLock::new();
    REGIONS.get_or_init(|| {
        serde_json::from_str(include_str!("../resources/download-regions.json"))
            .expect("bundled Steam download region catalog must be valid")
    })
}

/// `None` retains Steam's automatic server selection. Zero is the wire value
/// for automatic mode and must not be persisted as a named region.
pub fn validate_download_region(region_id: Option<u32>) -> Result<()> {
    if let Some(id) = region_id {
        ensure!(
            id != 0 && download_regions().iter().any(|region| region.id == id),
            "Неизвестный регион загрузки. Выберите регион из списка или автоматический выбор"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn bundled_catalog_has_unique_named_nonzero_cells() {
        let regions = download_regions();
        assert!(!regions.is_empty());
        let mut ids = HashSet::new();
        for region in regions {
            assert_ne!(region.id, 0);
            assert!(ids.insert(region.id), "duplicate Steam cell {}", region.id);
            assert!(!region.name.trim().is_empty());
            assert!(!region.name.chars().any(char::is_control));
        }
    }

    #[test]
    fn automatic_and_verified_regions_are_valid_but_unknown_ids_are_not() {
        assert!(validate_download_region(None).is_ok());
        for region in download_regions() {
            assert!(validate_download_region(Some(region.id)).is_ok());
        }
        assert!(validate_download_region(Some(0)).is_err());
        assert!(validate_download_region(Some(u32::MAX)).is_err());
    }
}
