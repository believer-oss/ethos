use crate::engine::CommunicationType;
use crate::engine::UnrealEngineProvider;
use directories_next::ProjectDirs;
use lazy_static::lazy_static;
use parking_lot::RwLock;
use regex::Regex;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::io::BufWriter;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
use std::time::SystemTime;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;

#[cfg(windows)]
use {ethos_core::CREATE_NO_WINDOW, std::os::windows::process::CommandExt};

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

#[derive(Debug, Serialize, Deserialize)]
struct TranslatedName {
    name: String,
    last_used_timestamp: SystemTime,
}

#[derive(Debug)]
pub struct OFPANameCache {
    path_to_name_map: HashMap<String, TranslatedName>,
    cmd_request_id: RwLock<u32>,
}

impl Default for OFPANameCache {
    fn default() -> Self {
        Self::new()
    }
}

fn get_cache_file_path() -> PathBuf {
    let proj_dirs = ProjectDirs::from("", "", crate::APP_NAME).expect("Unable to get project dirs");
    let mut path = proj_dirs.data_dir().to_path_buf();
    path.push("ofpa_cache.bin");
    path
}

impl OFPANameCache {
    pub fn new() -> Self {
        Self {
            path_to_name_map: HashMap::new(),
            cmd_request_id: RwLock::new(0),
        }
    }

    fn path_needs_translate(path: &str) -> bool {
        path.contains("Content/__ExternalActors__/")
            || path.contains("Content/__ExternalObjects__/")
    }

    fn add_name(&mut self, file: &str, asset_name: &str, timestamp: SystemTime) {
        let mut display_name = asset_name.to_string();
        if let Some(caps) = OFPA_PATH_REGEX.captures(file) {
            let path_to_map = &caps[1];
            display_name = format!("Content/{}/{}", path_to_map, asset_name);
        }
        _ = self.path_to_name_map.insert(
            file.to_string(),
            TranslatedName {
                name: display_name,
                last_used_timestamp: timestamp,
            },
        );
    }

    pub fn load_cache(&mut self) -> anyhow::Result<()> {
        assert_eq!(self.path_to_name_map.len(), 0); // should only be calling this function on startup

        let path: PathBuf = get_cache_file_path();
        if let Ok(cache_file) = File::open(path) {
            let mut reader = BufReader::new(cache_file);
            let mut deserializer = rmp_serde::Deserializer::new(&mut reader);
            self.path_to_name_map =
                HashMap::<String, TranslatedName>::deserialize(&mut deserializer)?;
        }

        Ok(())
    }

    fn save_cache(&mut self) -> anyhow::Result<()> {
        // evict any entries older than 1 week - this ensures only relevant entries
        // keep getting saved out so the cache doesn't get too big
        let now = SystemTime::now();
        let week = Duration::from_secs(60 * 60 * 24 * 7);
        self.path_to_name_map.retain(|_path, name| {
            let elapsed = now
                .duration_since(name.last_used_timestamp)
                .unwrap_or_default();
            elapsed < week
        });

        let path: PathBuf = get_cache_file_path();
        let cache_file = File::create(path)?;
        let mut writer = BufWriter::new(cache_file);
        let mut serializer = rmp_serde::Serializer::new(&mut writer);
        self.path_to_name_map.serialize(&mut serializer)?;

        Ok(())
    }

    // NOTE: we take the OFPANameCache by the Arc<RwLock> ref because we don't want to hold the lock
    // on the cache the entire time this function is running, as it could take a while.
    pub async fn get_names(
        provider: UnrealEngineProvider,
        communication: CommunicationType,
        engine_path: &Path,
        paths: &[String],
    ) -> Vec<String> {
        let is_editor_running = provider.is_editor_process_running();
        let now = SystemTime::now();

        let mut paths_to_request: Vec<String> = Vec::with_capacity(paths.len());
        {
            let cache = provider.ofpa_cache.read();
            for path in paths {
                let neeeds_translate = Self::path_needs_translate(path);
                // If the editor is running, the user could have changed the name of the asset, so attempt to fetch an updated
                // name for it if a refresh is desired. If the editor is closed, the last name we have in the cache is probably
                //  good enough since we run status updates pretty often.
                let needs_request = is_editor_running || !cache.path_to_name_map.contains_key(path);

                if neeeds_translate && needs_request {
                    paths_to_request.push(path.clone())
                }
            }
        }

        if !paths_to_request.is_empty() {
            // try to do a web request first, because it'll be faster than running the commandlet
            let mut web_request_succeeded = false;
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

                if let Ok(res) = res_or_err {
                    if res.status().is_client_error() {
                        let body = res.text().await.unwrap();
                        warn!(
                            "Got an error response. Falling back to commandlet. Error: {}",
                            body
                        );
                    } else {
                        match res.json::<OFPAFriendlyNamesResponse>().await {
                            Ok(data) => {
                                let mut cache = provider.ofpa_cache.write();
                                for item in data.names {
                                    if item.error.is_empty() {
                                        cache.add_name(&item.file_path, &item.asset_name, now);
                                    } else {
                                        debug!(
                                            "Error translating file path {}: {}",
                                            item.file_path, item.error
                                        );
                                    }
                                }
                                web_request_succeeded = true;
                            }
                            Err(e) => {
                                warn!(
                                    "Failed to unpack json response. Falling back to commandlet. Error: {}", e
                                );
                            }
                        };
                    }
                }
            }

            let should_try_commandlet =
                !web_request_succeeded && communication == CommunicationType::OfflineFallback;

            if should_try_commandlet {
                // We pass the list of requests to the Unreal commandlet by file, because there can be so many file paths that
                // they overflow the commandline limits.
                let mut listfile_path = std::env::temp_dir();
                listfile_path.push("Friendshipper");

                if !listfile_path.exists() {
                    if let Err(e) = std::fs::create_dir(&listfile_path) {
                        error!(
                            "Failed to create directory: {:?}. Reason: {}",
                            listfile_path, e
                        );
                    }
                }

                // There can be multiple get_names() requests going at the same time, so make sure they don't stomp on each other
                {
                    let cache = provider.ofpa_cache.read();
                    let mut cmd_request_id = cache.cmd_request_id.write();
                    *cmd_request_id += 1;

                    listfile_path.push(format!("ofpa_names_request_{}.txt", cmd_request_id));
                }

                let is_listfile_valid: bool = {
                    match File::create(&listfile_path) {
                        Err(e) => {
                            error!("Failed to create listfile '{:?}' for TranslateOFPAFilenames commandlet, unable to translate names. Error: {}", listfile_path, e);
                            false
                        }
                        Ok(file) => {
                            let mut writer = std::io::BufWriter::new(file);
                            for path in paths_to_request {
                                if let Err(e) = writeln!(writer, "{}", &path) {
                                    warn!(
                                        "Failed to write string '{}' to file {:?}. Reason: {}",
                                        path, listfile_path, e
                                    );
                                }
                            }
                            match writer.flush() {
                                Err(e) => {
                                    error!("Failed to write listfile '{:?}' for TranslateOFPAFilenames commandlet, unable to translate names. Error: {}", listfile_path, e);
                                    false
                                }
                                Ok(()) => true,
                            }
                        }
                    }
                };

                if is_listfile_valid {
                    let mut editor_dir: PathBuf = PathBuf::from(engine_path);
                    editor_dir.push("Engine/Binaries/Win64");

                    let mut editor_exe: PathBuf = editor_dir.clone();
                    editor_exe.push("UnrealEditor-Cmd");

                    let listfile_path_str = listfile_path.to_string_lossy();

                    let mut cmd = Command::new(editor_exe);
                    cmd.current_dir(&editor_dir);
                    cmd.arg(provider.uproject_path);
                    cmd.arg("-unattended");
                    cmd.arg("-Run=TranslateOFPAFilenames");
                    cmd.arg(format!("-ListFile=\'{}\'", listfile_path_str));

                    #[cfg(windows)]
                    cmd.creation_flags(CREATE_NO_WINDOW);

                    info!("Running Unreal commandlet: {:?}", cmd);

                    match cmd.output() {
                        Ok(output) => {
                            if output.status.success() {
                                let mut cache = provider.ofpa_cache.write();
                                let stdout = String::from_utf8(output.stdout).unwrap_or_default();
                                for line in stdout.lines() {
                                    if line.contains(
                                        "LogFriendshipperTranslateOFPAFilenamesCommandlet",
                                    ) {
                                        if let Some(caps) =
                                            OFPA_FRIENDLYNAME_LOG_SUCCESS_REGEX.captures(line)
                                        {
                                            let file = &caps[1];
                                            let name = &caps[2];
                                            cache.add_name(file, name, now);
                                        }
                                        if let Some(caps) =
                                            OFPA_FRIENDLYNAME_ERROR_REGEX.captures(line)
                                        {
                                            debug!(
                                                "Failed translating OFPA path. error: {}",
                                                caps[1].to_string()
                                            );
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

                // decrement the counter now that we don't need the file anymore
                {
                    let cache = provider.ofpa_cache.read();
                    let mut cmd_request_id = cache.cmd_request_id.write();
                    *cmd_request_id -= 1;
                }
            }
        }

        {
            let mut names = vec![];
            let mut cache = provider.ofpa_cache.write();

            for path in paths {
                if Self::path_needs_translate(path) {
                    let name = match cache.path_to_name_map.get_mut(path) {
                        Some(name) => {
                            name.last_used_timestamp = now;
                            name.name.clone()
                        }
                        None => String::new(),
                    };
                    names.push(name);
                } else {
                    names.push(String::new());
                }
            }

            if let Err(e) = cache.save_cache() {
                warn!("Failed to save OFPA cache: {}", e);
            }

            names
        }
    }
}
