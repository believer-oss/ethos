use ethos_core::types::config::UProject;
use lazy_static::lazy_static;
use parking_lot::RwLock;
use regex::Regex;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use sysinfo::{ProcessRefreshKind, System, UpdateKind};
use tracing::error;
use tracing::info;
use tracing::warn;

#[cfg(windows)]
use anyhow::bail;
#[cfg(windows)]
use ethos_core::CREATE_NO_WINDOW;
#[cfg(windows)]
use std::path::PathBuf;

pub type OFPANameCacheRef = std::sync::Arc<RwLock<OFPANameCache>>;

lazy_static! {
    static ref OFPA_FRIENDLYNAME_LOG_SUCCESS_REGEX: Regex =
        Regex::new(r"Display: (.+) has friendly name (.+)").unwrap();
    static ref OFPA_FRIENDLYNAME_ERROR_REGEX: Regex = Regex::new(r"Warning: (.+)").unwrap();

    // OFPA paths are always of the form: Content/<external_folder>/<path_to_map>/<toplevel>/<sublevel>/<hash>.uasset
    // Content/__ExternalActors__/Developers/MyCoolUser/TestMap/D/WI/YRCTUWELZX2XNF9YULI5OS.uasset
    static ref OFPA_PATH_REGEX: Regex = Regex::new(r"Content/\w+/([\w\d\/]+)/\w+/\w+/\w+.uasset").unwrap();
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OFPAFriendlyNamesRequest {
    filenames: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OFPAFriendlyNamesResponseItem {
    file_path: String,
    asset_name: String,
    error: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OFPAFriendlyNamesResponse {
    names: Vec<OFPAFriendlyNamesResponseItem>,
}

#[derive(Debug)]
pub struct OFPANameCache {
    pub path_to_name_map: HashMap<String, String>,
}

impl Default for OFPANameCache {
    fn default() -> Self {
        Self::new()
    }
}

impl OFPANameCache {
    pub fn new() -> Self {
        Self {
            path_to_name_map: HashMap::new(),
        }
    }

    fn path_needs_translate(path: &str) -> bool {
        path.contains("Content/__ExternalActors__/")
            || path.contains("Content/__ExternalObjects__/")
    }

    fn add_name(&mut self, file: &str, asset_name: &str) {
        let mut display_name = asset_name.to_string();
        if let Some(caps) = OFPA_PATH_REGEX.captures(file) {
            let path_to_map = &caps[1];
            display_name = format!("Content/{}/{}", path_to_map, asset_name);
        }
        _ = self.path_to_name_map.insert(file.to_string(), display_name);
    }

    // NOTE: we take the OFPANameCache by the Arc<RwLock> ref because we don't want to hold the lock
    // on the cache the entire time this function is running, as it could take a while.
    pub async fn get_names(
        cache_ref: OFPANameCacheRef,
        repo_path: &Path,
        uproject_path: &Path,
        engine_path: &Path,
        paths: &[String],
    ) -> Vec<String> {
        let is_editor_running = is_editor_process_running(repo_path);

        let mut paths_to_request: Vec<String> = Vec::with_capacity(paths.len());
        {
            let cache = cache_ref.read();
            for path in paths {
                let neeeds_translate = Self::path_needs_translate(path);
                // If the editor is running, the user could have changed the name of the asset, so attempt to fetch an updated
                // name for it. If the editor is closed, the last name we have in the cache is probably good enough since we
                // run status updates pretty often.
                let needs_request = is_editor_running || !cache.path_to_name_map.contains_key(path);
                if neeeds_translate && needs_request {
                    paths_to_request.push(path.clone())
                }
            }
        }

        if !paths_to_request.is_empty() {
            let mut should_try_commandlet = false;

            // try to do a web request first, because it'll be faster than running the commandlet
            if is_editor_running {
                let request_data = OFPAFriendlyNamesRequest {
                    filenames: paths_to_request.clone(),
                };

                let client = reqwest::Client::new();
                let res_or_err = client
                    .post("http://localhost:8091/friendshipper-ue/ofpa/friendlynames".to_string())
                    .json(&request_data)
                    .send()
                    .await;

                match res_or_err {
                    Ok(res) => {
                        if res.status().is_client_error() {
                            let body = res.text().await.unwrap();
                            warn!(
                                "Got an error response. Falling back to commandlet. Error: {}",
                                body
                            );
                            should_try_commandlet = true;
                        } else {
                            match res.json::<OFPAFriendlyNamesResponse>().await {
                                Ok(data) => {
                                    let mut cache = cache_ref.write();
                                    for item in data.names {
                                        if item.error.is_empty() {
                                            cache.add_name(&item.file_path, &item.asset_name);
                                        } else {
                                            warn!("{}", item.error);
                                        }
                                    }
                                }
                                Err(e) => {
                                    warn!(
                                        "Failed to unpack json response. Falling back to commandlet. Error: {}", e
                                    );
                                    should_try_commandlet = true;
                                }
                            };
                        }
                    }
                    Err(_) => should_try_commandlet = true,
                }
            } else {
                should_try_commandlet = true;
            }

            #[cfg(windows)]
            if should_try_commandlet {
                let mut editor_dir: PathBuf = PathBuf::from(engine_path);
                editor_dir.push("Engine\\Binaries\\Win64");

                let mut editor_debug_exe: PathBuf = editor_dir.clone();
                editor_debug_exe.push("UnrealEditor-Win64-DebugGame-Cmd.exe");

                let mut editor_dev_exe: PathBuf = editor_dir.clone();
                editor_dev_exe.push("UnrealEditor-Cmd.exe");

                // If the DebugGame exe exists, the user is likely an engineer iterating on code, so check and see which exe is newer and use that one.
                // Note that this is an ungodly amount of nesting but is the simplest way to check the two filetimes without doing a bunch of unwraps.
                let editor_exe: PathBuf = 'exe: {
                    if let Ok(editor_debug_exe_metadata) = std::fs::metadata(&editor_debug_exe) {
                        if let Ok(editor_dev_exe_metadata) = std::fs::metadata(&editor_dev_exe) {
                            if let Ok(debug_modified) = editor_debug_exe_metadata.modified() {
                                if let Ok(dev_modified) = editor_dev_exe_metadata.modified() {
                                    if let Ok(debug_elapsed) = debug_modified.elapsed() {
                                        if let Ok(dev_elapsed) = dev_modified.elapsed() {
                                            if debug_elapsed < dev_elapsed {
                                                break 'exe editor_debug_exe;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    editor_dev_exe
                };

                let mut cmd = Command::new(editor_exe);
                cmd.current_dir(&editor_dir);
                cmd.arg(uproject_path);
                cmd.arg("-Run=TranslateOFPAFilenames");
                for path in paths_to_request {
                    cmd.arg(path);
                }

                #[cfg(windows)]
                cmd.creation_flags(CREATE_NO_WINDOW);

                match cmd.output() {
                    Ok(output) => {
                        if output.status.success() {
                            let mut cache = cache_ref.write();
                            let stdout = String::from_utf8(output.stdout).unwrap_or_default();
                            for line in stdout.lines() {
                                if line.contains("LogFriendshipperTranslateOFPAFilenamesCommandlet")
                                {
                                    if let Some(caps) =
                                        OFPA_FRIENDLYNAME_LOG_SUCCESS_REGEX.captures(line)
                                    {
                                        let file = &caps[1];
                                        let name = &caps[2];
                                        cache.add_name(file, name);
                                    }
                                    if let Some(caps) = OFPA_FRIENDLYNAME_ERROR_REGEX.captures(line)
                                    {
                                        warn!("{}", caps[1].to_string());
                                    }
                                }
                            }
                        } else {
                            let stderr = String::from_utf8(output.stderr).unwrap_or_default();
                            error!(
                                "Failed to run TranslateOFPAFilenames commandlet. Error output:\n{}",
                                stderr
                            );
                        }
                    }
                    Err(e) => warn!("Error running commandlet: {}", e),
                }
            }
        }

        let cache = cache_ref.read();

        let mut names = vec![];
        for path in paths {
            if Self::path_needs_translate(path) {
                let name = match cache.path_to_name_map.get(path) {
                    Some(path) => path.clone(),
                    None => String::new(),
                };
                names.push(name);
            } else {
                names.push(String::new());
            }
        }
        names
    }
}

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
