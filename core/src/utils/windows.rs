use anyhow::{anyhow, Result};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Storage::FileSystem::{DefineDosDeviceW, DEFINE_DOS_DEVICE_FLAGS};

// See: https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-definedosdevicew
pub fn mount_drive(drive: &str, target: &str) -> Result<()> {
    unsafe {
        DefineDosDeviceW(
            // No flags
            DEFINE_DOS_DEVICE_FLAGS(0),
            &HSTRING::from(drive),
            &HSTRING::from(target),
        )
        .map_err(|e| anyhow!("Failed to mount drive: {:?}", e))
    }
}

pub fn unmount_drive(drive: &str) -> Result<()> {
    unsafe {
        DefineDosDeviceW(
            // 0x2 DDD_REMOVE_DEFINITION
            DEFINE_DOS_DEVICE_FLAGS(0x2),
            &HSTRING::from(drive),
            PCWSTR::null(),
        )
        .map_err(|e| anyhow!("Failed to unmount drive: {:?}", e))
    }
}
