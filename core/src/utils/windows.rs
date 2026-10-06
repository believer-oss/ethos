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

/// Which programs currently have any of `paths` open.
///
/// This answers the question the location check cannot. A program running from inside a
/// directory obviously holds its own files, but Unreal Editor runs from the *engine*
/// directory while holding the *repo's* DLLs open - so where it runs from says nothing
/// about which binaries it has loaded, and that is precisely the case that breaks editor
/// binary syncs.
///
/// Uses the Restart Manager, which is the mechanism Windows installers use to produce
/// "close these applications before continuing". Asking the OS which process holds a
/// handle is exact, where guessing from process names is not.
///
/// Best-effort throughout: every failure returns nothing. This only ever improves an
/// error message that the caller already has, and a worse message is not worth turning
/// into a failure of its own.
#[cfg(target_os = "windows")]
pub fn programs_holding(paths: &[std::path::PathBuf]) -> Vec<String> {
    use std::os::windows::ffi::OsStrExt;

    use tracing::debug;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::RestartManager::{
        RmEndSession, RmGetList, RmRegisterResources, RmStartSession, CCH_RM_SESSION_KEY,
        RM_PROCESS_INFO,
    };

    /// Bounds the allocation. Far more than the handful of programs anyone has open, and
    /// a list longer than this would be unreadable anyway.
    const MAX_HOLDERS: usize = 64;

    if paths.is_empty() {
        return Vec::new();
    }

    // These must outlive the pointer array built from them.
    let wide: Vec<Vec<u16>> = paths
        .iter()
        .map(|path| {
            path.as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        })
        .collect();
    let file_names: Vec<PCWSTR> = wide.iter().map(|w| PCWSTR(w.as_ptr())).collect();

    let mut session: u32 = 0;
    let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];

    // SAFETY: `key` is the documented size, and `session` is written only on success.
    let started = unsafe { RmStartSession(&mut session, 0, PWSTR(key.as_mut_ptr())) };
    if started != ERROR_SUCCESS {
        debug!("Could not start a Restart Manager session: {started:?}");
        return Vec::new();
    }

    let holders = (|| {
        // SAFETY: `file_names` borrows `wide`, which is alive for this whole closure.
        let registered = unsafe { RmRegisterResources(session, Some(&file_names), None, None) };
        if registered != ERROR_SUCCESS {
            debug!("Could not register files with the Restart Manager: {registered:?}");
            return Vec::new();
        }

        let mut needed: u32 = 0;
        let mut count: u32 = MAX_HOLDERS as u32;
        let mut reasons: u32 = 0;
        let mut buffer: Vec<RM_PROCESS_INFO> = vec![RM_PROCESS_INFO::default(); MAX_HOLDERS];

        // SAFETY: `buffer` holds `count` elements, which is the count passed in.
        let listed = unsafe {
            RmGetList(
                session,
                &mut needed,
                &mut count,
                Some(buffer.as_mut_ptr()),
                &mut reasons,
            )
        };

        if listed != ERROR_SUCCESS {
            // Including ERROR_MORE_DATA: more holders than the buffer took, and the docs
            // do not promise what is in it then. Naming none is better than naming
            // whatever happens to be in uninitialised-ish memory - the caller falls back
            // to listing the usual suspects.
            debug!("Could not list the programs holding these files: {listed:?} (needed {needed})");
            return Vec::new();
        }

        buffer.truncate(count as usize);
        let found = buffer;

        let mut names: Vec<String> = found
            .iter()
            .map(|process| {
                let raw = &process.strAppName;
                let len = raw.iter().position(|c| *c == 0).unwrap_or(raw.len());
                String::from_utf16_lossy(&raw[..len])
            })
            .filter(|name| !name.is_empty())
            .collect();

        // One line per program: several editor windows is still one thing to close.
        names.sort();
        names.dedup();
        names
    })();

    // SAFETY: `session` was started successfully above, and is ended exactly once.
    let _ = unsafe { RmEndSession(session) };

    holders
}

/// Nothing outside Windows holds files the way this is asking about.
#[cfg(not(target_os = "windows"))]
pub fn programs_holding(_paths: &[std::path::PathBuf]) -> Vec<String> {
    Vec::new()
}
