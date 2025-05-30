use crate::state::{AppState, FrontendOp};

use crate::engine::EngineProvider;
use crate::system::git::{configure_user, install};
use crate::system::logs::{get_logs, open_system_logs_folder};
use crate::system::terminal::open_terminal_to_path;
use axum::extract::State;
use axum::routing::{get, post};
use axum::Json;
use axum::Router;
use ethos_core::types::config::UnrealVerSelDiagResponse;
use ethos_core::types::errors::CoreError;
use std::fs;
use tracing::{debug, info};

use super::unreal::check_unreal_file_association;

pub fn router<T>() -> Router<AppState<T>>
where
    T: EngineProvider,
{
    Router::new()
        .route("/show-ui", post(show_ui))
        .route("/git/configure", post(configure_user))
        .route("/git/install", post(install))
        .route("/logs", get(get_logs))
        .route("/open-logs", post(open_system_logs_folder))
        .route("/terminal", post(open_terminal_to_path))
        .route("/status", get(status))
        .route(
            "/diagnostics/unrealversionselector",
            get(get_unrealversionselector_diags),
        )
}

async fn status() -> String {
    String::from("OK")
}

async fn show_ui<T>(State(state): State<AppState<T>>)
where
    T: EngineProvider,
{
    state
        .frontend_op_tx
        .send(FrontendOp::ShowUI)
        .expect("show UI failed somehow");
}

async fn get_unrealversionselector_diags<T>(
    State(_state): State<AppState<T>>,
) -> Result<Json<UnrealVerSelDiagResponse>, CoreError>
where
    T: EngineProvider,
{
    info!("Checking UnrealVersionSelector.exe and .uproject file association");
    // Check for the existence of UnrealVersionSelector
    let (valid_version_selector, version_selector_msg) = {
        let path = std::path::PathBuf::from("C:\\")
            .join("Program Files (x86)")
            .join("Epic Games")
            .join("Launcher")
            .join("Engine")
            .join("Binaries")
            .join("Win64")
            .join("UnrealVersionSelector.exe");
        match fs::metadata(&path) {
            Ok(metadata) => {
                if metadata.is_file() {
                    (true, "UnrealVersionSelector.exe: found".to_string())
                } else {
                    (false, format!("{} is not a file!", path.display()))
                }
            }
            Err(e) => (false, format!("UnrealVersionSelector.exe: {e}")),
        }
    };

    debug!("valid_version_selector: {}", valid_version_selector);
    debug!("version_selector_msg: {}", version_selector_msg);

    info!("Checking .uproject file association");
    let (uproject_file_assoc, uproject_file_assoc_msg) = check_unreal_file_association()?;

    debug!("uproject_file_assoc: {}", uproject_file_assoc);
    debug!("uproject_file_assoc_msg: {:?}", uproject_file_assoc_msg);

    let json = Json(UnrealVerSelDiagResponse {
        valid_version_selector,
        version_selector_msg,
        uproject_file_assoc,
        uproject_file_assoc_msg,
    });

    Ok(json)
}
