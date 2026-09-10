use core::cmp::Ordering;
#[cfg(unix)]
use std::os::unix::prelude::PermissionsExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::process::Child;
use std::sync::Arc;
use std::time::SystemTime;
use std::{
    env,
    fs::{self, File},
    io::{copy, BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::Sender,
};

use anyhow::{anyhow, Context, Result};
use aws_credential_types::Credentials;
use chrono::{DateTime, Utc};
use directories_next::ProjectDirs;
use hex::FromHex;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use tracing::{error, info, instrument, warn};
use which::{which, which_in};

use super::fs::LocalDownloadPath;
use super::msg::LongtailMsg;
use crate::clients::aws::AWSClient;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

// Send a Msg down the transmit channel
pub fn send_msg(tx: &Sender<LongtailMsg>, msg: LongtailMsg) {
    if tx.send(msg.clone()).is_err() {
        // We probably can't and don't want to do much in the way of error handling,
        // since we're likely on a thread. At worst the UI gets a little wonky, because
        // log messages and the busy spinner stops working...
        info!("Failed to send message! {:?} {:?}", tx, msg)
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct Longtail {
    pub app_name: String,
    pub exec_path: Option<PathBuf>,
    pub download_path: LocalDownloadPath,

    #[serde(skip)]
    pub child_process: Arc<Mutex<Option<Child>>>,
}

pub struct CacheControl {
    pub path: PathBuf,
    pub max_size_bytes: u64,
}

/// How many times a download may restart after its credentials lapsed. Each restart
/// resumes from the cache.
const MAX_CREDENTIAL_RETRIES: usize = 5;

/// Diagnostics kept from a failed run, capped because longtail's output is mostly
/// progress bars.
const MAX_ERROR_RECORDS: usize = 5;
const MAX_ERROR_RECORD_CHARS: usize = 1500;

/// Errors S3 reports when it rejects a request outright rather than mid-transfer.
const CREDENTIAL_ERROR_MARKERS: [&str; 4] = [
    "ExpiredToken",
    "ExpiredTokenException",
    "InvalidAccessKeyId",
    "token has expired",
];

/// Recovery steps for a failed download, in order of how much work they throw away.
/// Clearing the cache costs the entire download, so credential failures never reach it.
enum Recovery {
    ClearTarget,
    ClearCache,
}

/// Whether an attempt failed over credentials rather than anything on disk. A lapse
/// mid-download surfaces only as a block store I/O error, so the expiry is the more
/// reliable signal.
fn credentials_lapsed(error: &str, expires_at: Option<DateTime<Utc>>) -> bool {
    if CREDENTIAL_ERROR_MARKERS
        .iter()
        .any(|marker| error.contains(marker))
    {
        return true;
    }

    matches!(expires_at, Some(expires_at) if expires_at <= Utc::now())
}

fn same_session(a: &Credentials, b: &Credentials) -> bool {
    a.access_key_id() == b.access_key_id() && a.session_token() == b.session_token()
}

/// Split the error and fatal records out of a chunk of longtail output. The progress bar
/// shares the chunk, and the record naming the failure comes last.
fn error_records(chunk: &str) -> Vec<String> {
    let mut records: Vec<String> = vec![];
    let mut rest = chunk;

    while let Some(start) = ["level=error", "level=fatal"]
        .iter()
        .filter_map(|marker| rest.find(marker))
        .min()
    {
        rest = &rest[start..];
        let end = rest[1..].find("level=").map_or(rest.len(), |i| i + 1);
        records.push(
            rest[..end]
                .trim()
                .chars()
                .take(MAX_ERROR_RECORD_CHARS)
                .collect(),
        );
        rest = &rest[end..];
    }

    records
}

/// Keep the most recent records: the failure that stopped the run is reported last.
fn keep_error_records(kept: &mut Vec<String>, records: Vec<String>) {
    for record in records {
        if record.trim().is_empty() {
            continue;
        }

        kept.push(record);
        if kept.len() > MAX_ERROR_RECORDS {
            kept.remove(0);
        }
    }
}

struct FileCacheData {
    path: PathBuf,
    size: u64,
    timestamp: SystemTime,
}

impl FileCacheData {
    fn collect(path: &Path) -> Vec<FileCacheData> {
        let mut vec: Vec<FileCacheData> = vec![];
        Self::collect_internal(path, &mut vec);
        vec
    }

    fn collect_internal(path: &Path, data: &mut Vec<FileCacheData>) {
        if let Ok(dir_entries) = fs::read_dir(path) {
            for entry in dir_entries.flatten() {
                if let Ok(metadata) = entry.metadata() {
                    if metadata.is_dir() {
                        Self::collect_internal(&entry.path(), data);
                    } else if metadata.is_file() {
                        let last_modified = metadata.modified().unwrap();
                        let last_accessed = metadata.accessed().unwrap();

                        let timestamp = last_modified.max(last_accessed);

                        data.push(FileCacheData {
                            path: entry.path().clone(),
                            size: metadata.len(),
                            timestamp,
                        });
                    }
                }
            }
        }
    }
}

impl Longtail {
    pub fn new(app_name: &str) -> Self {
        let exec_path = Longtail::find_exec(app_name);

        Longtail {
            exec_path,
            app_name: app_name.to_string(),
            download_path: LocalDownloadPath::new(app_name),
            child_process: Arc::new(Mutex::new(None)),
        }
    }

    // Per-platform executable names, defaulting to a renamed 'longtail' binary
    fn get_longtail_exec_name() -> String {
        match std::env::consts::OS {
            "linux" => "longtail-linux-x64",
            "macos" => "longtail-macos-x64",
            "windows" => "longtail-win32-x64.exe",
            _ => "longtail",
        }
        .to_string()
    }

    // Build the URL to download longtail from
    fn get_longtail_dl_url() -> String {
        let exec_name = Longtail::get_longtail_exec_name();

        format!(
            "{}/{}/{}",
            crate::LONGTAIL_DL_PREFIX,
            crate::LONGTAIL_VERSION,
            exec_name
        )
    }

    // Search for the longtail executable in our download dir or the user's path
    #[instrument]
    fn find_exec(app_name: &str) -> Option<PathBuf> {
        let exe_name = Longtail::get_longtail_exec_name();

        // Try to find the executable in the project data path, and if that fails
        // check the current exe directory.
        let mut exe_path: Option<PathBuf>;
        if let Some(proj_dirs) = ProjectDirs::from("", "", app_name) {
            exe_path = Some(proj_dirs.data_dir().to_path_buf());
        } else {
            exe_path = env::current_exe().ok().or(None);
            if let Some(exe_path) = &mut exe_path {
                exe_path.pop();
            };
        }

        // Check the path found above, and if all else fails try to find it in $PATH
        match which_in(exe_name.clone(), exe_path, env::current_dir().unwrap()) {
            Ok(path) => Some(path),
            Err(_) => which(exe_name).ok(),
        }
    }

    // Wrapper for find_exec to update the struct
    #[instrument(err)]
    pub fn update_exec(&mut self) -> Result<()> {
        match Self::find_exec(&self.app_name) {
            Some(path) => self.exec_path = Some(path),
            None => {
                return Err(anyhow!("Could not find longtail executable!"));
            }
        };
        Ok(())
    }

    // Download the longtail executable and check it's hash
    #[instrument(skip(tx), err)]
    pub fn get_longtail(&self, tx: Sender<LongtailMsg>) -> Result<()> {
        // First try to use the data_dir, and if we can't use the curent exe's path
        let mut exe_path: PathBuf;
        if let Some(proj_dirs) = ProjectDirs::from("", "", &self.app_name) {
            exe_path = proj_dirs.data_dir().to_path_buf();
        } else {
            exe_path = env::current_exe().context("Could not find current path!!!")?;
        }
        exe_path.push(Longtail::get_longtail_exec_name());

        let url = Longtail::get_longtail_dl_url();
        send_msg(&tx, LongtailMsg::ExecEvt(format!("{url:?}")));

        let response = ureq::get(&url).call()?;
        let mut dest = {
            send_msg(&tx, LongtailMsg::Log(format!("dl_path: [{exe_path:?}]")));
            tracing::info!("[longtail] get_longtail dl_path: [{:?}]", exe_path);

            let exe_dir = exe_path.parent().unwrap();
            let _ = std::fs::create_dir_all(exe_dir);
            File::create(exe_path.clone())?
        };
        let bytes = copy(&mut response.into_reader(), &mut dest)?;
        send_msg(&tx, LongtailMsg::Log(format!("Copied {bytes} bytes")));

        send_msg(&tx, LongtailMsg::DoneLtDlEvt);

        let mut hasher = Sha256::new();
        let file = File::open(exe_path.clone())?;
        let mut reader = BufReader::new(file);
        let bytes = copy(&mut reader, &mut hasher)?;
        let hash = hasher.finalize();

        if *hash != <[u8; 32]>::from_hex(crate::LONGTAIL_SHA256).unwrap() {
            send_msg(
                &tx,
                LongtailMsg::ErrEvt(format!("Failed to validate hash! {bytes}/{hash:x}")),
            );
            send_msg(&tx, LongtailMsg::Log("Deleting file!".to_string()));
            match fs::remove_file(exe_path.clone()) {
                Ok(_) => info!("Successfully remove unverified longtail executable"),
                Err(e) => warn!("Unable to remove unverified longtail executable! {:?}", e),
            };
        }

        send_msg(
            &tx,
            LongtailMsg::Log(format!("Hash from {bytes} bytes: {hash:x}")),
        );

        #[cfg(unix)]
        {
            let mut perms = fs::metadata(exe_path.clone())?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(exe_path.clone(), perms)?;
        }

        Ok(())
    }

    // Get the current longtail executable version
    #[instrument(skip(tx), err)]
    pub fn get_version(&self, tx: Sender<LongtailMsg>) -> Result<()> {
        let cmd = self.exec_path.clone().context("No exec path set")?;

        let output = Command::new(cmd).arg("version").output()?;
        if !output.status.success() {
            return Err(anyhow!("Command executed with failing exit code"));
        };
        send_msg(&tx, LongtailMsg::Log(String::from_utf8(output.stdout)?));
        Ok(())
    }

    // Use longtail 'get' to download a given archive
    #[instrument(skip(cache, tx, aws_client), err)]
    pub fn get_archive(
        &self,
        path: &Path,
        cache: Option<CacheControl>,
        archives: &[String],
        tx: Sender<LongtailMsg>,
        aws_client: &AWSClient,
    ) -> Result<()> {
        info!(
            "Attempting to download longtail archives {:?} to path {:?}",
            &archives, path
        );

        let Some(cache) = cache else {
            let (credentials, _) = aws_client.current_credentials();
            return self.get_archive_internal(path, None, archives, &tx, &credentials);
        };

        let mut recovery = [Recovery::ClearTarget, Recovery::ClearCache].into_iter();
        let mut credential_retries = 0;

        // Read per attempt: a child process can only be handed credentials when it is
        // spawned, so outliving the session means retrying with the renewed one.
        let result = loop {
            let (credentials, expires_at) = aws_client.current_credentials();
            let err =
                match self.get_archive_internal(path, Some(&cache), archives, &tx, &credentials) {
                    Ok(()) => break Ok(()),
                    Err(e) => e,
                };

            if credentials_lapsed(&err.to_string(), expires_at) {
                let (renewed, _) = aws_client.current_credentials();
                if same_session(&credentials, &renewed) {
                    break Err(anyhow!(
                        "AWS credentials expired during download and have not been renewed. \
                         Sign in again and restart the download - cached chunks are kept, so \
                         it resumes rather than starting over. Original error: {}",
                        err
                    ));
                }

                if credential_retries >= MAX_CREDENTIAL_RETRIES {
                    break Err(anyhow!(
                        "AWS credentials expired {} times without the download completing. \
                         Original error: {}",
                        credential_retries,
                        err
                    ));
                }

                credential_retries += 1;
                warn!(
                    "Longtail get failed after AWS credentials expired mid-download (retry {} of \
                     {}). Retrying with renewed credentials, keeping the target path and cache so \
                     the download resumes. Original error was: {:?}",
                    credential_retries, MAX_CREDENTIAL_RETRIES, err
                );
                continue;
            }

            match recovery.next() {
                Some(Recovery::ClearTarget) => {
                    warn!("Longtail get failed. Attempting to clear target path and retry unpack. Original error was: {:?}", err);
                    if path.exists() {
                        std::fs::remove_dir_all(path)?;
                    }
                }
                Some(Recovery::ClearCache) => {
                    warn!("Longtail get failed AGAIN - assuming bad chunks in cache. Attempting to clear cache and retry download + unpack. Original error was: {:?}", err);
                    std::fs::remove_dir_all(&cache.path)?;
                }
                None => break Err(err),
            }
        };

        info!("Longtail get result: {:?}", result);
        result
    }

    pub fn get_archive_internal(
        &self,
        path: &Path,
        cache: Option<&CacheControl>,
        archives: &[String],
        tx: &Sender<LongtailMsg>,
        credentials: &Credentials,
    ) -> Result<()> {
        let cmd = self.exec_path.clone().context("No exec path set")?;
        let pipe = Stdio::piped();
        let errpipe = Stdio::piped();

        let mut exec = Command::new(cmd);
        #[cfg(windows)]
        exec.creation_flags(CREATE_NO_WINDOW);

        exec.arg("get");
        for archive in archives {
            exec.arg("--source-path").arg(archive);
        }
        exec.arg("--target-path")
            .arg(path.as_os_str())
            .env("AWS_DEFAULT_REGION", crate::AWS_REGION);

        if let Some(cache) = &cache {
            exec.arg("--cache-path").arg(&cache.path);
        }

        send_msg(tx, LongtailMsg::ExecEvt(format!("{exec:?}")));

        // Add creds separately so they aren't logged in the above msg
        exec.env("AWS_ACCESS_KEY_ID", credentials.access_key_id())
            .env("AWS_SECRET_ACCESS_KEY", credentials.secret_access_key())
            .env(
                "AWS_SESSION_TOKEN",
                credentials.session_token().unwrap_or(""),
            );

        let mut output = exec.stdout(pipe).stderr(errpipe).spawn()?;

        let stdout = output.stdout.take().expect("Failed to get stdout!!!");
        let stderr = output.stderr.take().expect("Failed to get stderr!!!");

        self.child_process.lock().replace(output);

        let reader = BufReader::new(stdout);
        let errreader = BufReader::new(stderr);

        let mut error_records_kept: Vec<String> = vec![];

        // Longtail is using hardcoded CR characters in it's progress bar implementation, so
        // split on those... https://github.com/DanEngelbrecht/golongtail/blob/main/longtailutils/progress.go#L24
        reader
            // .lines()
            .split(b'\r')
            .filter_map(|line| line.ok())
            .for_each(|line| {
                let line = std::str::from_utf8(&line).unwrap_or("").replace('\n', "");
                if !line.is_empty() {
                    // longtail logs its fatals to stdout, not stderr.
                    keep_error_records(&mut error_records_kept, error_records(&line));
                    send_msg(tx, LongtailMsg::Log(line));
                }
            });

        errreader
            .lines()
            .map_while(|line| line.ok())
            .for_each(|line| {
                send_msg(tx, LongtailMsg::ErrEvt(line.clone()));
                keep_error_records(&mut error_records_kept, vec![line]);
            });

        let mut child = self
            .child_process
            .lock()
            .take()
            .expect("No child process found");

        let status = child
            .wait()
            .map_err(|e| anyhow::anyhow!("Failed waiting on child: {}", e))?;

        if !status.success() {
            let error_message = error_records_kept.join("\n");
            return Err(anyhow::anyhow!(
                "Longtail command failed: {}",
                error_message
            ));
        }

        send_msg(tx, LongtailMsg::DoneArcSyncEvt);

        if let Some(cache) = &cache {
            let mut all_files = FileCacheData::collect(&cache.path.join("chunks"));
            let total_size = all_files.iter().fold(0, |acc, entry| acc + entry.size);

            if total_size > cache.max_size_bytes {
                info!("File cache total size {} is over threshold {} by {} bytes. Purging old chunks...",
                    total_size, cache.max_size_bytes, total_size - cache.max_size_bytes);
                all_files.sort_by(|a, b| -> Ordering {
                    let time_ord = a.timestamp.partial_cmp(&b.timestamp);
                    if time_ord == Some(Ordering::Equal) {
                        return a.size.partial_cmp(&b.size).unwrap();
                    }
                    time_ord.unwrap()
                });

                let mut current_size = total_size;
                for f in all_files.iter() {
                    info!(
                        "Deleting chunk {:?} with size {} (total {} -> {}, threshold {})",
                        f.path,
                        f.size,
                        current_size,
                        current_size - f.size,
                        cache.max_size_bytes
                    );
                    if let Err(e) = std::fs::remove_file(&f.path) {
                        warn!("Unable to delete file {:?}: {:?}", &f.path, e);
                    }
                    current_size -= f.size;
                    if current_size <= cache.max_size_bytes {
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    #[instrument]
    pub fn log_message(msg: LongtailMsg) {
        match msg {
            LongtailMsg::Log(s) => {
                info!("Log: {}", s);
            }
            LongtailMsg::ExecEvt(s) => {
                info!("Executing: {}", s)
            }
            LongtailMsg::ErrEvt(s) => {
                error!("ERROR: {}", s)
            }
            LongtailMsg::DoneLtDlEvt => {
                info!("Done downloading longtail");
            }
            LongtailMsg::DoneArcSyncEvt => {
                info!("Done syncing");
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shape of a real failure: the progress bar shares the chunk, the cause comes last.
    const LAPSED_MID_DOWNLOAD: &str = concat!(
        "Updating version           100%: |####|: [42m18s]        ",
        r#"level=error msg="job_api->WaitForAllJobs() failed with 5" line=1049"#,
        r#"level=error msg="Longtail_RunJobsBatched() failed with 5" line=8851"#,
        "Dropping prefetched block due to background prefetch",
        r#"level=fatal msg="get: downsync: Failed writing version to `C:\engine`: ChangeVersion2: 5: I/O error.""#,
    );

    const REJECTED_UP_FRONT: &str = concat!(
        r#"level=fatal msg="get: ReadFromURI: operation error S3: GetObject, https "#,
        r#"response error StatusCode: 400, api error ExpiredToken: The provided token has expired.""#,
    );

    fn creds(access_key: &str, session_token: &str) -> Credentials {
        Credentials::from_keys(access_key, "secret", Some(session_token.to_string()))
    }

    #[test]
    fn error_records_skips_progress_and_keeps_the_cause() {
        let records = error_records(LAPSED_MID_DOWNLOAD);

        assert_eq!(records.len(), 3);
        assert!(records[0].starts_with("level=error"));
        assert!(records[2].contains("I/O error"));
        assert!(records.iter().all(|r| !r.contains("Updating version")));
    }

    #[test]
    fn error_records_ignores_output_without_a_failure() {
        assert!(error_records("Updating version  59%: |###|: [37m10s:28m30s]").is_empty());
        assert!(error_records(r#"level=warning msg="Dropping prefetched block""#).is_empty());
    }

    #[test]
    fn keep_error_records_keeps_the_last_ones() {
        let mut kept = vec![];
        for i in 0..MAX_ERROR_RECORDS + 3 {
            keep_error_records(&mut kept, vec![format!("record {i}")]);
        }

        assert_eq!(kept.len(), MAX_ERROR_RECORDS);
        assert_eq!(
            kept.last().unwrap(),
            &format!("record {}", MAX_ERROR_RECORDS + 2)
        );
    }

    #[test]
    fn rejected_request_reads_as_a_credential_failure() {
        // No expiry to go on: the message alone has to carry it.
        assert!(credentials_lapsed(REJECTED_UP_FRONT, None));
    }

    #[test]
    fn lapse_mid_download_reads_as_a_credential_failure() {
        // Only the expiry distinguishes a lapsed session from a corrupt cache here;
        // misreading it deletes the cache.
        let expired = Utc::now() - chrono::Duration::seconds(1);

        assert!(credentials_lapsed(LAPSED_MID_DOWNLOAD, Some(expired)));
    }

    #[test]
    fn io_error_within_a_live_session_is_not_a_credential_failure() {
        let valid = Utc::now() + chrono::Duration::hours(1);

        assert!(!credentials_lapsed(LAPSED_MID_DOWNLOAD, Some(valid)));
        assert!(!credentials_lapsed(LAPSED_MID_DOWNLOAD, None));
    }

    #[test]
    fn same_session_tracks_the_token() {
        let original = creds("AKIAEXAMPLE", "token-one");

        assert!(same_session(&original, &creds("AKIAEXAMPLE", "token-one")));
        assert!(!same_session(&original, &creds("AKIAEXAMPLE", "token-two")));
        assert!(!same_session(&original, &creds("AKIAOTHER", "token-one")));
    }
}
