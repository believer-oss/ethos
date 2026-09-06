use anyhow::anyhow;
use axum::extract::State;
use serde::Deserialize;
use tracing::{error, info, warn};

use ethos_core::types::errors::CoreError;

use crate::engine::EngineProvider;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
struct WindowsSdk {
    #[serde(rename = "VisualStudioSuggestedComponents")]
    visual_studio_suggested_components: Vec<String>,

    #[serde(rename = "VisualStudio2026SuggestedComponents")]
    visual_studio_2026_suggested_components: Vec<String>,

    #[serde(rename = "MinimumVisualStudio2026Version")]
    minimum_visual_studio_2026_version: String,
}

pub async fn install_build_tools_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    #[cfg(not(windows))]
    {
        return Err(CoreError::Internal(anyhow!(
            "Install Build Tools is only supported on Windows."
        )));
    }

    #[cfg(windows)]
    {
        use crate::repo::CREATE_NO_WINDOW;
        use tokio::process::Command;

        /// Query winget for available versions of a package, returned latest-first.
        async fn query_winget_versions(package_id: &str) -> Result<Vec<String>, CoreError> {
            let mut cmd = Command::new("winget");
            cmd.args(["show", "--id", package_id, "--versions", "--accept-source-agreements"]);
            cmd.creation_flags(CREATE_NO_WINDOW);

            let output = cmd.output().await.map_err(|e| {
                CoreError::Internal(anyhow!(
                    "Failed to run winget show --versions for {}: {}",
                    package_id,
                    e
                ))
            })?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(CoreError::Internal(anyhow!(
                    "winget show --versions for {} failed: {}",
                    package_id,
                    stderr
                )));
            }

            let stdout = String::from_utf8_lossy(&output.stdout);
            // Output format:
            //   Found <Name> [<Id>]
            //   Version
            //   ------
            //   18.9.1
            //   17.14.39
            //   ...
            // Skip lines until we pass the "------" separator, then collect versions.
            let mut past_separator = false;
            let mut versions: Vec<String> = Vec::new();
            for line in stdout.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("---") {
                    past_separator = true;
                    continue;
                }
                if past_separator && !trimmed.is_empty() {
                    versions.push(trimmed.to_string());
                }
            }

            info!(
                "Found {} available versions for {}",
                versions.len(),
                package_id
            );
            Ok(versions)
        }

        info!("Starting build tools installation");

        let (app_config, repo_config) = {
            let ac = state.app_config.read().clone();
            let rc = state.repo_config.read().clone();
            (ac, rc)
        };

        let engine_path = app_config
            .load_engine_path_from_repo(&repo_config)
            .map_err(|e| {
                CoreError::Internal(anyhow!(
                    "Failed to resolve engine path: {}. Ensure repo path and .uproject are configured.",
                    e
                ))
            })?;

        let sdk_json_path = engine_path
            .join("Engine")
            .join("Config")
            .join("Windows")
            .join("Windows_SDK.json");

        let sdk_contents = std::fs::read_to_string(&sdk_json_path).map_err(|e| {
            CoreError::Internal(anyhow!(
                "Failed to read {}: {}. Ensure the engine is downloaded and the file exists.",
                sdk_json_path.display(),
                e
            ))
        })?;

        let sdk: WindowsSdk = serde_json::from_str(&sdk_contents).map_err(|e| {
            CoreError::Internal(anyhow!(
                "Failed to parse {}: {}",
                sdk_json_path.display(),
                e
            ))
        })?;

        let mut errors: Vec<String> = Vec::new();

        // 1. Install Visual Studio Community with components from the SDK config.
        //    Queries winget for available versions and picks the latest at or above
        //    MinimumVisualStudio2026Version. Combines VisualStudioSuggestedComponents +
        //    VisualStudio2026SuggestedComponents into the --override argument.
        {
            let vs_versions =
                query_winget_versions("Microsoft.VisualStudio.Community").await?;

            // Pick the latest version >= minimum
            let vs_version = vs_versions
                .iter()
                .find(|v| v.as_str() >= sdk.minimum_visual_studio_2026_version.as_str())
                .map(|v| v.as_str());

            match vs_version {
                Some(version) => {
                    let mut override_parts: Vec<String> =
                        vec!["--quiet --wait --norestart --nocache".to_string()];

                    for component in sdk
                        .visual_studio_suggested_components
                        .iter()
                        .chain(sdk.visual_studio_2026_suggested_components.iter())
                    {
                        // Workload entries get ";includeRecommended", individual components do not
                        if component.contains("Workload.") {
                            override_parts
                                .push(format!("--add {component};includeRecommended"));
                        } else {
                            override_parts.push(format!("--add {component}"));
                        }
                    }

                    let override_arg = override_parts.join(" ");

                    info!(
                        "Installing Visual Studio Community {} (minimum: {}) with components",
                        version, sdk.minimum_visual_studio_2026_version
                    );

                    let args = [
                        "install",
                        "--id",
                        "Microsoft.VisualStudio.Community",
                        "--force",
                        "--exact",
                        "--silent",
                        "--accept-source-agreements",
                        "--accept-package-agreements",
                        "--version",
                        version,
                        "--override",
                        &override_arg,
                    ];

                    info!("winget {}", args.join(" "));

                    let mut cmd = Command::new("winget");
                    cmd.args(&args);
                    cmd.creation_flags(CREATE_NO_WINDOW);

                    match cmd.output().await {
                        Ok(output) => {
                            if !output.status.success() {
                                let stderr =
                                    String::from_utf8_lossy(&output.stderr);
                                let stdout =
                                    String::from_utf8_lossy(&output.stdout);
                                error!(
                                    "Visual Studio install failed: stdout={}, stderr={}",
                                    stdout, stderr
                                );
                                errors.push(format!("Visual Studio: {}", stderr));
                            } else {
                                info!("Successfully installed Visual Studio Community");
                            }
                        }
                        Err(e) => {
                            error!(
                                "Failed to execute winget for Visual Studio: {}",
                                e
                            );
                            errors.push(format!("Visual Studio: {}", e));
                        }
                    }
                }
                None => {
                    warn!(
                        "No Visual Studio Community version >= {} found in winget",
                        sdk.minimum_visual_studio_2026_version
                    );
                    errors.push(format!(
                        "Visual Studio: no version >= {} available",
                        sdk.minimum_visual_studio_2026_version
                    ));
                }
            }
        }

        // 2. Install Visual C++ Redistributable from the engine's bundled installer.
        {
            let vcredist_path = engine_path
                .join("Engine")
                .join("Extras")
                .join("Redist")
                .join("en-us")
                .join("vc_redist.x64.exe");

            if !vcredist_path.exists() {
                return Err(CoreError::Internal(anyhow!(
                    "VC++ Redistributable installer not found at {}. Ensure the engine is fully downloaded.",
                    vcredist_path.display()
                )));
            }

            info!("Installing VC++ Redistributable from {}", vcredist_path.display());

            let mut cmd = Command::new(&vcredist_path);
            cmd.args(["/install", "/quiet", "/norestart"]);
            cmd.creation_flags(CREATE_NO_WINDOW);

            match cmd.output().await {
                Ok(output) => {
                    if !output.status.success() {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        error!(
                            "VC++ Redistributable install failed (exit {}): stdout={}, stderr={}",
                            output.status, stdout, stderr
                        );
                        errors.push(format!("VC++ Redistributable: exit {}", output.status));
                    } else {
                        info!("Successfully installed VC++ Redistributable");
                    }
                }
                Err(e) => {
                    error!("Failed to execute VC++ Redistributable installer: {}", e);
                    errors.push(format!("VC++ Redistributable: {}", e));
                }
            }
        }

        if !errors.is_empty() {
            return Err(CoreError::Internal(anyhow!(
                "Some build tools failed to install:\n{}",
                errors.join("\n")
            )));
        }

        info!("All build tools installed successfully");
        Ok(())
    }
}
