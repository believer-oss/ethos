#[cfg(windows)]
use anyhow::bail;
use ethos_core::types::config::UProject;
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;
use sysinfo::{ProcessRefreshKind, System, UpdateKind};
use tracing::info;

pub fn is_editor_process_running(repo_path: &Path) -> bool {
    let mut system = System::new();
    let refresh_kind = ProcessRefreshKind::new().with_cmd(UpdateKind::OnlyIfNotSet);
    system.refresh_processes_specifics(refresh_kind);

    let repo_path: String = repo_path
        .to_str()
        .unwrap_or_default()
        .to_lowercase()
        .replace('\\', "/");

    for process in system.processes_by_name("UnrealEditor") {
        for arg in process.cmd() {
            let arg: String = arg.to_lowercase().replace('\\', "/");
            if arg.contains(&repo_path) {
                return true;
            }
        }
    }

    false
}

pub fn update_engine_association_registry(
    engine_path: &Path,
    new_uproject: &UProject,
    old_uproject: &Option<UProject>,
) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;

        let (builds_registry, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey("Software\\Epic Games\\Unreal Engine\\Builds")?;
        if let Some(old_uproject) = &old_uproject {
            if old_uproject.is_custom_engine() {
                _ = builds_registry.delete_value(&old_uproject.engine_association);
            }
        }

        // cleanup any keys that use the current engine path
        {
            let mut keys_to_delete: Vec<String> = vec![];
            for (name, value) in builds_registry.enum_values().map(|x| x.unwrap()) {
                // need to do this annoying conversion from a null-terminated u16 byte array to String
                let widechars: Vec<u16> = value
                    .bytes
                    .chunks_exact(2)
                    .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
                    .collect();

                let null_byte_index = widechars
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(widechars.len());
                let widechars_no_null: &[u16] = &widechars[0..null_byte_index];
                let os_string = OsString::from_wide(widechars_no_null);
                let value_as_str: String = os_string.into_string().unwrap_or_default();
                let value_as_str = value_as_str.replace('\\', "/");
                let value_as_path = PathBuf::from(value_as_str);
                if value_as_path == engine_path {
                    keys_to_delete.push(name);
                }
            }

            for name in keys_to_delete {
                let _ = builds_registry.delete_value(name);
            }
        }

        let engine_path: PathBuf = PathBuf::from(engine_path);

        if let Err(e) = builds_registry.set_value(
            &new_uproject.engine_association,
            &engine_path.clone().into_os_string().into_string().unwrap(),
        ) {
            bail!(
                "Failed to set engine association {} to {} in registry: {}",
                new_uproject.engine_association,
                engine_path.display(),
                e
            );
        } else {
            info!(
                "set engine association reg key {} to {}",
                new_uproject.engine_association,
                engine_path.display(),
            );
        }
    }

    #[cfg(not(windows))]
    info!(
        "Would've set engine association registry key {:?} to {:?} updating {:?}",
        engine_path, new_uproject, old_uproject
    );

    Ok(())
}

// Check for believable Unreal Engine file association registry keys
// Mirrors FDesktopPlatformWindows::GetRequiredRegistrySettings in
// Engine/Source/Developer/DesktopPlatform/Private/Windows/DesktopPlatformWindows.cpp
#[allow(clippy::vec_init_then_push)]
pub fn check_unreal_file_association() -> anyhow::Result<(bool, Vec<String>)> {
    #[cfg(windows)]
    let mut result = true;
    #[cfg(not(windows))]
    let result = true;

    let mut messages: Vec<String> = vec![];

    #[cfg(windows)]
    {
        // use std::ffi::OsString;
        // use std::os::windows::ffi::OsStringExt;
        use winreg::enums::{HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
        use winreg::RegKey;
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let _hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let hkcr = RegKey::predef(HKEY_CLASSES_ROOT);

        // Check the list of UE builds from the Epic Launcher
        {
            info!("Checking Unreal Engine builds in registry");
            let path = Path::new("Software")
                .join("EpicGames")
                .join("Unreal Engine");
            let keys = hkcu.open_subkey(path);
            while let Ok(ref key) = keys {
                let engine_builds = key.enum_keys().map(|x| x.unwrap());
                for engine_build in engine_builds {
                    let engine_build_key = key.open_subkey(&engine_build).unwrap();
                    let engine_path: String =
                        engine_build_key.get_value("InstalledDirectory").unwrap();
                    let engine_path = Path::new(&engine_path);
                    if !engine_path.exists() {
                        messages.push(format!(
                            "Engine build {} at {} does not exist",
                            engine_build,
                            engine_path.display()
                        ));
                        result = false;
                    }
                }
            }
        }

        // Check that the .uproject is associated with Unreal.ProjectFile
        {
            info!("Checking .uproject file association");
            let uproject_key = hkcr.open_subkey(".uproject");
            if uproject_key.is_err() {
                messages.push("No .uproject key found".to_string());
                result = false;
            } else {
                let uproject_key = uproject_key.unwrap();
                let value: String = uproject_key.get_value("").unwrap();
                if value != "Unreal.ProjectFile" {
                    messages.push(format!(
                        ".uproject key is set to {} instead of Unreal.ProjectFile",
                        value
                    ));
                    result = false;
                }
            }
        }

        // Check that the Unreal.ProjectFile association is set to a valid path
        {
            info!("Checking Unreal.ProjectFile association");
            let path = Path::new("Unreal.ProjectFile")
                .join("shell")
                .join("open")
                .join("command");
            // let project_file_key = hkcr.open_subkey("Unreal.ProjectFile");
            let project_file_key = hkcr.open_subkey(path);
            if project_file_key.is_err() {
                messages.push("No Unreal.ProjectFile key found".to_string());
                result = false;
            } else {
                let project_file_key = project_file_key.unwrap();
                let value: String = project_file_key.get_value("").unwrap();
                let value = value.split('"').nth(1).unwrap();
                let value = Path::new(&value);
                if !value.exists() {
                    messages.push(format!(
                        "Unreal.ProjectFile key is set to {} which does not exist",
                        value.display()
                    ));
                    result = false;
                }
            }
        }

        // Check that the HKCU Explorer FileExts are set
        {
            info!("Checking Windows Explorer .uproject file association");
            let path = Path::new("Software")
                .join("Microsoft")
                .join("Windows")
                .join("CurrentVersion")
                .join("Explorer")
                .join("FileExts")
                .join(".uproject")
                .join("OpenWithProgids");
            match hkcu.open_subkey(path) {
                Ok(key) => {
                    if !key
                        .enum_values()
                        .map(|x| x.unwrap().0)
                        .any(|x| &x == "Unreal.ProjectFile")
                    {
                        messages.push(
                            "HKCU .uproject FileExts key is not set to Unreal.ProjectFile"
                                .to_string(),
                        );
                        result = false;
                    };
                }
                Err(_) => {
                    messages.push("No HKCU .uproject key found".to_string());
                    result = false;
                }
            }
        }

        // See if anything exists under UserChoice
        {
            info!("Checking Windows Explorer .uproject UserChoice key");
            let path = PathBuf::from("Software")
                .join("Microsoft")
                .join("Windows")
                .join("CurrentVersion")
                .join("Explorer")
                .join("FileExts")
                .join(".uproject")
                .join("UserChoice");
            let key = hkcu.open_subkey(path);
            // If the key exists, it should be Unreal.ProjectFile
            if key.is_ok()
                && key.unwrap().get_value::<String, _>("ProgId").unwrap() != "Unreal.ProjectFile"
            {
                messages.push(
                    "HKCU .uproject UserChoice key is not set to Unreal.ProjectFile".to_string(),
                );
                result = false;
            }
        }
    }

    #[cfg(not(windows))]
    messages.push("Would've checked registry keys if we were running on windows.".to_string());

    Ok((result, messages))
}
