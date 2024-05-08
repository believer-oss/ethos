use crate::state::AppState;
use crate::state::FrontendOp;

use crate::system::git::{configure_user, install};
use crate::system::logs::{get_logs, open_system_logs_folder};
use axum::extract::State;
use axum::routing::{get, post};
use axum::Json;
use axum::Router;
use ethos_core::types::config::UnrealVerSelDiagResponse;
use ethos_core::types::errors::CoreError;
use std::fs;
use std::sync::Arc;
use tracing::{debug, info};

use super::unreal::check_unreal_file_association;
use super::update::{get_latest_version, run_update};

pub fn router(shared_state: Arc<AppState>) -> Router {
    Router::new()
        .route("/show-ui", post(show_ui))
        .route("/git/configure", post(configure_user))
        .route("/git/install", post(install))
        .route("/logs", get(get_logs))
        .route("/open-logs", post(open_system_logs_folder))
        .route("/status", get(status))
        .route("/update", get(get_latest_version).post(run_update))
        .route(
            "/diagnostics/unrealversionselector",
            get(get_unrealversionselector_diags),
        )
        .with_state(shared_state)
}

async fn status() -> String {
    String::from("OK")
}

async fn show_ui(State(state): State<Arc<AppState>>) {
    state
        .frontend_op_tx
        .send(FrontendOp::ShowUI)
        .expect("show UI failed somehow");
}

async fn get_unrealversionselector_diags(
    State(_state): State<Arc<AppState>>,
) -> Result<Json<UnrealVerSelDiagResponse>, CoreError> {
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
            Err(e) => (false, format!("UnrealVersionSelector.exe: {}", e)),
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
