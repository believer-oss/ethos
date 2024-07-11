use std::path::PathBuf;

use anyhow::anyhow;
use anyhow::bail;
use axum::{
    extract::{Query, State},
    routing::post,
    Router,
};
use serde::Deserialize;
use serde::Serialize;
use tokio::process::Command;
use tracing::{error, info};

use crate::engine::EngineProvider;
use ethos_core::clients::git;
use ethos_core::clients::git::CommitFormat;
use ethos_core::clients::git::CommitHead;
use ethos_core::clients::git::ShouldPrune;
use ethos_core::types::config::EngineType;
use ethos_core::types::config::UProject;
use ethos_core::types::errors::CoreError;

use crate::repo::operations;
#[cfg(windows)]
use crate::repo::CREATE_NO_WINDOW;
use crate::state::AppState;
use crate::system::unreal;

#[derive(Default, Deserialize)]
struct SolutionParams {
    #[serde(default)]
    generate: bool,

    #[serde(default)]
    open: bool,
}

pub fn router<T>() -> Router<AppState<T>>
where
    T: EngineProvider,
{
    Router::new()
        .route("/sln", post(sln_handler))
        .route("/open-project", post(open_project))
        .route(
            "/install-git-hooks",
            post(operations::install_git_hooks_handler),
        )
        .route(
            "/sync-engine-commit-with-uproject",
            post(sync_engine_commit_with_uproject),
        )
        .route(
            "/sync-uproject-commit-with-engine",
            post(sync_uproject_commit_with_engine),
        )
}

async fn open_project<T>(State(state): State<AppState<T>>) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    state.engine.open_project().await.map_err(CoreError)
}

async fn sln_handler<T>(
    State(state): State<AppState<T>>,
    solution: Query<SolutionParams>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let res = generate_and_open_project(&state, &solution).await;
    match res {
        Err(e) => Err(CoreError(e)),
        Ok(_) => Ok(()),
    }
}

async fn generate_and_open_project<T>(
    state: &AppState<T>,
    solution: &SolutionParams,
) -> anyhow::Result<()>
where
    T: EngineProvider,
{
    let project_relative_path = state.repo_config.read().uproject_path.clone();

    let solution_relative_path = project_relative_path.replace(".uproject", ".sln");

    if solution_relative_path.is_empty() {
        bail!("No repo path set. Please set the path to the game repo in Preferences.");
    }

    let repo_path = state.app_config.read().repo_path.clone();
    let project_path = PathBuf::from(repo_path.clone()).join(project_relative_path);
    let uproject = UProject::load(&project_path).unwrap();
    let engine_path = state.app_config.read().get_engine_path(&uproject);
    let solution_path = PathBuf::from(repo_path).join(solution_relative_path);

    if solution.generate {
        info!("generating project files for UProject: {:?}", uproject);

        #[cfg(target_os = "windows")]
        let build_bat_path: PathBuf = engine_path.join("Engine/Build/BatchFiles/Build.bat");
        #[cfg(target_os = "macos")]
        let build_bat_path = engine_path.join("Engine/Build/BatchFiles/Mac/Build.sh");
        #[cfg(target_os = "linux")]
        let build_bat_path = engine_path.join("Engine/Build/BatchFiles/Linux/Build.sh");

        let mut cmd = Command::new(build_bat_path);

        cmd.arg("-projectfiles")
            .arg(format!("-project={}", project_path.to_str().unwrap()))
            .arg("-game")
            .arg("-rocket")
            .arg("-progress");

        if state.app_config.read().engine_type == EngineType::Source {
            cmd.arg("-engine");
        }

        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);

        match cmd.output().await {
            Ok(output) => {
                if !output.status.success() {
                    error!(
                        "Failed to generate solution: {}",
                        String::from_utf8_lossy(&output.stdout)
                    );
                    bail!("Failed to generate solution. Check log for details.");
                }
            }
            Err(e) => {
                error!("Failed to generate solution: {}", e);
                bail!("Failed to generate solution. Check log for details.");
            }
        }
    }

    if solution.open {
        match open::that(solution_path) {
            Ok(_) => (),
            Err(e) => {
                bail!("Failed to open solution: {}", e);
            }
        }
    }

    Ok(())
}

async fn sync_engine_commit_with_uproject<T>(
    State(state): State<AppState<T>>,
) -> Result<String, CoreError>
where
    T: EngineProvider,
{
    info!("Syncing engine commit with uproject.");

    let app_config = state.app_config.read().clone();

    if app_config.engine_type != EngineType::Source {
        return Err(CoreError(anyhow!(
            "Preferences are configured to use prebuilt engine."
        )));
    }

    let repo_config = state.repo_config.read().clone();
    let uproject_path = app_config.get_uproject_path(&repo_config);
    let uproject = UProject::load(&uproject_path)?;

    if !uproject.is_custom_engine() {
        return Err(CoreError(anyhow!(
            "UProject is not configured to use a custom engine."
        )));
    }

    let engine_commit: String = match uproject.get_custom_engine_sha() {
        Ok(sha) => sha,
        Err(e) => {
            error!("Caught error parsing engine sha: {}", e);
            return Err(CoreError(anyhow!(
                "Caught error parsing engine commit. Check log for details."
            )));
        }
    };

    let engine_path: PathBuf = app_config.get_engine_path(&uproject);

    info!(
        "Attempting to update engine repo at {:?} to commit {}",
        engine_path, engine_commit
    );

    let git_client = git::Git::new(engine_path, state.git_tx.clone());
    git_client.fetch(ShouldPrune::No).await?;
    git_client.checkout(&engine_commit).await?;

    let solution = SolutionParams {
        generate: true,
        open: false,
    };
    if let Err(e) = generate_and_open_project(&state, &solution).await {
        return Err(CoreError(e));
    }

    Ok(engine_commit)
}

async fn sync_uproject_commit_with_engine<T>(
    State(state): State<AppState<T>>,
) -> Result<String, CoreError>
where
    T: EngineProvider,
{
    info!("Syncing uproject with current engine commit.");

    let app_config = state.app_config.read().clone();

    if app_config.engine_type != EngineType::Source {
        return Err(CoreError(anyhow!(
            "Preferences are configured to use prebuilt engine."
        )));
    }

    let repo_config = state.repo_config.read().clone();
    let uproject_path = app_config.get_uproject_path(&repo_config);
    let old_uproject = UProject::load(&uproject_path)?;
    if !old_uproject.is_custom_engine() {
        return Err(CoreError(anyhow!(
            "UProject is configured to use stock Unreal version {}, not a custom version.",
            old_uproject.engine_association
        )));
    }

    let engine_path: PathBuf = app_config.get_engine_path(&old_uproject);
    let git_client = git::Git::new(engine_path.clone(), state.git_tx.clone());
    let engine_commit = git_client
        .head_commit(CommitFormat::Short, CommitHead::Local)
        .await?;

    if let Ok(uproject_commit) = old_uproject.get_custom_engine_sha() {
        if uproject_commit == engine_commit {
            info!(
                "UProject and engine {:?} have same commit {} - skipping update.",
                engine_path, engine_commit
            );
            return Ok(engine_commit);
        }
    }

    let uproject_contents: String = match std::fs::read_to_string(&uproject_path) {
        Ok(s) => s,
        Err(e) => {
            error!(
                "Failed to read uproject from file {:?}: {}",
                uproject_path, e
            );
            return Err(CoreError(anyhow!(
                "Failed to read UProject file {:?}. Check log for details.",
                uproject_path
            )));
        }
    };

    let mut uproject_json: serde_json::Value = match serde_json::from_str(&uproject_contents) {
        Ok(contents) => contents,
        Err(e) => {
            error!(
                "Failed to parse uprojects contents into json. Error: {}. Contents: {}",
                e, uproject_contents
            );
            return Err(CoreError(anyhow!(
                "Failed to parse uproject json. Check log for details.",
            )));
        }
    };

    let engine_association = format!("believer-{}", engine_commit);

    info!(
        "Updating uproject EngineAssociation at path {:?} to {}",
        uproject_path, engine_association
    );

    uproject_json["EngineAssociation"] = serde_json::Value::String(engine_association);

    // we have to do all this stuff instead of just calling serde_json::to_string_pretty() just to
    // preserve the tabs :/
    let updated_contents = {
        let formatter = serde_json::ser::PrettyFormatter::with_indent(b"\t");
        let mut updated_contents_buffer = Vec::new();
        let mut serializer =
            serde_json::Serializer::with_formatter(&mut updated_contents_buffer, formatter);
        uproject_json.serialize(&mut serializer).unwrap();
        String::from_utf8(updated_contents_buffer).unwrap()
    };

    if let Err(e) = std::fs::write(&uproject_path, updated_contents) {
        error!(
            "Failed to write uproject file {:?}. Error: {}",
            uproject_path, e
        );
        return Err(CoreError(anyhow!(
            "Failed to write uproject file. Check log for details."
        )));
    }

    // update registry since the engine association is different now
    let new_uproject = UProject::load(&uproject_path)?;
    unreal::update_engine_association_registry(&engine_path, &new_uproject, &Some(old_uproject))?;

    let solution = SolutionParams {
        generate: true,
        open: false,
    };
    if let Err(e) = generate_and_open_project(&state, &solution).await {
        return Err(CoreError(e));
    }

    Ok(engine_commit)
}
