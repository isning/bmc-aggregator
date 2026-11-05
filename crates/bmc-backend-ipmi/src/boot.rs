use async_trait::async_trait;
use bmc_feature_api::boot::{
    BootBackend, BootCapabilities, BootEntry, BootEntryDiscoveryState, BootMode, BootSelection,
};

use crate::client::{blocking_client, checked_raw};
use crate::IpmiConfig;

pub struct IpmiBootBackend {
    config: IpmiConfig,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BootTarget {
    Default,
    Pxe,
    Disk,
    SafeMode,
    Diagnostic,
    Optical,
    BiosSetup,
    RemoteRemovable,
    RemoteOptical,
    PrimaryRemote,
    RemoteDisk,
    Floppy,
}

const BOOT_TARGETS: [BootTarget; 12] = [
    BootTarget::Default,
    BootTarget::Pxe,
    BootTarget::Disk,
    BootTarget::SafeMode,
    BootTarget::Diagnostic,
    BootTarget::Optical,
    BootTarget::BiosSetup,
    BootTarget::RemoteRemovable,
    BootTarget::RemoteOptical,
    BootTarget::PrimaryRemote,
    BootTarget::RemoteDisk,
    BootTarget::Floppy,
];

impl IpmiBootBackend {
    pub fn new(config: IpmiConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl BootBackend for IpmiBootBackend {
    async fn capabilities(&self) -> BootCapabilities {
        BootCapabilities {
            entries: category_entries(),
            entry_discovery: BootEntryDiscoveryState::Unsupported,
        }
    }

    async fn selection(&self) -> Result<BootSelection, String> {
        let config = crate::client::resolve_config(&self.config).await?;
        tokio::task::spawn_blocking(move || {
            let client = blocking_client(&config).map_err(|error| error.to_string())?;
            let data = checked_raw(
                client
                    .send_raw(0x00, 0x09, &[5, 0, 0])
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            parse_boot_selection(&data)
        })
        .await
        .map_err(|error| format!("IPMI boot task failed: {error}"))?
    }

    async fn set_selection(&self, selection: BootSelection) -> Result<(), String> {
        let (target, efi) = selection
            .entry_id
            .as_deref()
            .and_then(parse_entry_id)
            .ok_or_else(|| "unknown IPMI boot entry".to_string())?;
        let config = crate::client::resolve_config(&self.config).await?;
        tokio::task::spawn_blocking(move || {
            let client = blocking_client(&config).map_err(|error| error.to_string())?;
            let mut flags = if target == BootTarget::Default {
                0
            } else {
                0x80
            };
            if selection.mode == BootMode::Persistent {
                flags |= 0x40;
            }
            if efi {
                flags |= 0x20;
            }
            let data = [5, flags, boot_target_selector(target) << 2, 0, 0, 0];
            checked_raw(
                client
                    .send_raw(0x00, 0x08, &data)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            let readback = checked_raw(
                client
                    .send_raw(0x00, 0x09, &[5, 0, 0])
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            let observed = parse_boot_selection(&readback)?;
            use bmc_backend_model::transaction::{Event, State};
            let matches = observed.entry_id == Some(category_entry(target, efi).id)
                && observed.mode == selection.mode;
            if State::Sending
                .step(Event::Completion(0))
                .step(Event::Observed(matches))
                != State::Confirmed
            {
                return Err("IPMI boot readback mismatch; remote result unknown".to_string());
            }
            Ok(())
        })
        .await
        .map_err(|error| format!("IPMI boot task failed: {error}"))?
    }
}

fn category_entries() -> Vec<BootEntry> {
    [false, true]
        .into_iter()
        .flat_map(|efi| {
            BOOT_TARGETS
                .into_iter()
                .map(move |target| category_entry(target, efi))
        })
        .collect()
}

fn category_entry(target: BootTarget, efi: bool) -> BootEntry {
    let (id, label) = match target {
        BootTarget::Default => ("default", "Default"),
        BootTarget::Pxe => ("pxe", "Network (PXE)"),
        BootTarget::Disk => ("disk", "Local disk"),
        BootTarget::SafeMode => ("safe_mode", "Local disk (safe mode)"),
        BootTarget::Diagnostic => ("diagnostic", "Diagnostic partition"),
        BootTarget::Optical => ("optical", "Optical drive"),
        BootTarget::BiosSetup => ("bios_setup", "BIOS setup"),
        BootTarget::RemoteRemovable => ("remote_removable", "Remote removable"),
        BootTarget::RemoteOptical => ("remote_optical", "Remote optical"),
        BootTarget::PrimaryRemote => ("primary_remote", "Primary remote media"),
        BootTarget::RemoteDisk => ("remote_disk", "Remote disk"),
        BootTarget::Floppy => ("floppy", "Floppy"),
    };
    BootEntry {
        id: format!("ipmi:target:{id}:{}", if efi { "uefi" } else { "legacy" }),
        label: label.to_string(),
        group: Some("Boot targets".to_string()),
        badges: vec![if efi { "UEFI" } else { "Legacy" }.to_string()],
        enabled: true,
        selectable: true,
        supported_modes: vec![BootMode::Once, BootMode::Persistent],
    }
}

fn parse_entry_id(entry_id: &str) -> Option<(BootTarget, bool)> {
    BOOT_TARGETS.into_iter().find_map(|target| {
        [false, true]
            .into_iter()
            .find(|efi| category_entry(target, *efi).id == entry_id)
            .map(|efi| (target, efi))
    })
}

fn boot_target_selector(target: BootTarget) -> u8 {
    match target {
        BootTarget::Default => 0x0,
        BootTarget::Pxe => 0x1,
        BootTarget::Disk => 0x2,
        BootTarget::SafeMode => 0x3,
        BootTarget::Diagnostic => 0x4,
        BootTarget::Optical => 0x5,
        BootTarget::BiosSetup => 0x6,
        BootTarget::RemoteRemovable => 0x7,
        BootTarget::RemoteOptical => 0x8,
        BootTarget::PrimaryRemote => 0x9,
        BootTarget::RemoteDisk => 0xb,
        BootTarget::Floppy => 0xf,
    }
}

fn selector_boot_target(selector: u8) -> Option<BootTarget> {
    Some(match selector {
        0x0 => BootTarget::Default,
        0x1 => BootTarget::Pxe,
        0x2 => BootTarget::Disk,
        0x3 => BootTarget::SafeMode,
        0x4 => BootTarget::Diagnostic,
        0x5 => BootTarget::Optical,
        0x6 => BootTarget::BiosSetup,
        0x7 => BootTarget::RemoteRemovable,
        0x8 => BootTarget::RemoteOptical,
        0x9 => BootTarget::PrimaryRemote,
        0xb => BootTarget::RemoteDisk,
        0xf => BootTarget::Floppy,
        _ => return None,
    })
}

fn parse_boot_selection(data: &[u8]) -> Result<BootSelection, String> {
    if data.len() < 7 || data[1] & 0x80 != 0 {
        return Err("invalid boot flags response".to_string());
    }
    let flags = data[2];
    let target = if flags & 0x80 == 0 {
        BootTarget::Default
    } else {
        selector_boot_target((data[3] >> 2) & 0x0f)
            .ok_or_else(|| "unknown boot target selector".to_string())?
    };
    Ok(BootSelection {
        mode: if flags & 0x40 != 0 {
            BootMode::Persistent
        } else {
            BootMode::Once
        },
        entry_id: Some(category_entry(target, flags & 0x20 != 0).id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn boot_flags_decode_target_mode_and_efi() {
        let selection = parse_boot_selection(&[1, 0, 0xe0, 0x14, 0, 0, 0]).unwrap();
        assert_eq!(selection.mode, BootMode::Persistent);
        assert_eq!(
            selection.entry_id.as_deref(),
            Some("ipmi:target:optical:uefi")
        );
        assert_eq!(
            parse_entry_id(selection.entry_id.as_deref().unwrap()),
            Some((BootTarget::Optical, true))
        );
    }

    #[test]
    fn boot_entries_expose_display_group_and_firmware_badge() {
        let legacy = category_entry(BootTarget::Default, false);
        let uefi = category_entry(BootTarget::Default, true);
        assert_eq!(legacy.label, "Default");
        assert_eq!(uefi.label, "Default");
        assert_eq!(legacy.group.as_deref(), Some("Boot targets"));
        assert_eq!(legacy.badges, vec!["Legacy"]);
        assert_eq!(uefi.badges, vec!["UEFI"]);
    }

    #[test]
    fn boot_device_types_group_legacy_before_uefi() {
        let entries = category_entries();
        assert_eq!(entries.len(), BOOT_TARGETS.len() * 2);
        assert!(entries[..BOOT_TARGETS.len()]
            .iter()
            .all(|entry| entry.badges == ["Legacy"]));
        assert!(entries[BOOT_TARGETS.len()..]
            .iter()
            .all(|entry| entry.badges == ["UEFI"]));
        assert_eq!(entries[0].label, "Default");
        assert_eq!(entries[BOOT_TARGETS.len()].label, "Default");
    }
}
