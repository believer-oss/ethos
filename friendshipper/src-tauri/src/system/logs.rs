use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader};

use anyhow::anyhow;
use axum::extract::State;
use axum::Json;
use tracing::error;

use crate::engine::EngineProvider;
use ethos_core::types::errors::CoreError;
use ethos_core::types::logs::LogEntry;

use crate::state::AppState;

pub async fn get_logs<T>(State(state): State<AppState<T>>) -> Result<Json<Vec<LogEntry>>, CoreError>
where
    T: EngineProvider,
{
    let mut results = vec![];
    let log_path = state.log_path.clone();

    let last_modified_log = fs::read_dir(log_path)
        .expect("Unable to read log directory")
        .flatten()
        .filter(|f| f.metadata().unwrap().is_file())
        .max_by_key(|f| f.metadata().unwrap().modified().unwrap())
        .unwrap();

    let file = match File::open(last_modified_log.path()) {
        Ok(file) => file,
        Err(e) => {
            return Err(anyhow!("Error opening log file: {:?}", e).into());
        }
    };

    for line in BufReader::new(file).lines() {
        let line = line.unwrap();
        let log_entry: LogEntry = match serde_json::from_str(&line) {
            Ok(log_entry) => log_entry,
            Err(e) => {
                error!("Error parsing log entry: {:?}, line: {}", e, line.as_str());
                return Err(anyhow!("Error parsing log entry: {:?}", e).into());
            }
        };

        results.push(log_entry);
    }

    results.reverse();

    Ok(Json(results))
}

pub async fn open_system_logs_folder<T>(State(state): State<AppState<T>>)
where
    T: EngineProvider,
{
    let log_path = state.log_path.clone();
    if let Err(e) = open::that(log_path.as_os_str()) {
        error!("Failed to open logs folder: {:?}", e);
    }
}
