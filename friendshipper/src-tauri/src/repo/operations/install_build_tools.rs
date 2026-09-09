use anyhow::anyhow;
use axum::extract::State;
#[cfg(windows)]
use serde::Deserialize;
#[cfg(windows)]
use tracing::{error, info, warn};

use ethos_core::types::errors::CoreError;

use crate::engine::EngineProvider;
use crate::state::AppState;

#[cfg(windows)]
#[derive(Debug, Deserialize)]
struct WindowsSdk {
    #[serde(rename = "VisualStudioSuggestedComponents")]
    visual_studio_suggested_components: Vec<String>,

    #[serde(rename = "VisualStudio2026SuggestedComponents")]
    visual_studio_2026_suggested_components: Vec<String>,

    #[serde(rename = "MinimumVisualStudio2026Version")]
    minimum_visual_studio_2026_version: String,
}

#[cfg(windows)]
impl WindowsSdk {
    /// Returns an iterator over all suggested Visual Studio components.
    fn all_components(&self) -> impl Iterator<Item = &String> {
        self.visual_studio_suggested_components
            .iter()
            .chain(self.visual_studio_2026_suggested_components.iter())
    }
}

/// Returns true if the component ID contains only safe characters
/// (alphanumeric, dots, hyphens, underscores).
#[cfg(windows)]
fn is_valid_component_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
}

/// Parse a dot-separated version string into numeric components for correct
/// ordering (e.g. "17.5.0" < "17.14.0").
#[cfg(windows)]
fn parse_version(version: &str) -> Vec<u64> {
    version
        .split('.')
        .filter_map(|s| s.parse::<u64>().ok())
        .collect()
}

pub async fn install_build_tools_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    #[cfg(not(windows))]
    {
        let _ = state;
        Err(CoreError::Internal(anyhow!(
            "Install Build Tools is only supported on Windows."
        )))
    }

    #[cfg(windows)]
    install_build_tools_windows(state).await
}

#[cfg(windows)]
static INSTALLING_BUILD_TOOLS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Guard that resets the [`INSTALLING_BUILD_TOOLS`] flag when dropped.
#[cfg(windows)]
struct InstallGuard;

#[cfg(windows)]
impl Drop for InstallGuard {
    fn drop(&mut self) {
        INSTALLING_BUILD_TOOLS.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Winget/installer exit code indicating a reboot is required.
#[cfg(windows)]
const EXIT_CODE_REBOOT_REQUIRED: i32 = 3010;

/// Installer exit code indicating the same or a newer version is already installed.
#[cfg(windows)]
const EXIT_CODE_ALREADY_INSTALLED: i32 = 1638;

/// Winget exit code when a package is already installed and no upgrade is available.
#[cfg(windows)]
const EXIT_CODE_NO_UPGRADE_FOUND: i32 = -1978335189;

/// Winget exit code when no applicable installer is found (also returned when
/// the installer is blocked from running, e.g. UAC denied).
#[cfg(windows)]
const EXIT_CODE_NO_APPLICABLE_INSTALLER: i32 = -1978335226;

#[cfg(windows)]
const WINGET_QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Query winget for the currently installed version of a package, if any.
#[cfg(windows)]
async fn query_installed_version(package_id: &str) -> Result<Option<String>, CoreError> {
    use crate::repo::CREATE_NO_WINDOW;
    use tokio::process::Command;

    let mut cmd = Command::new("winget");
    cmd.args([
        "list",
        "--id",
        package_id,
        "--exact",
        "--accept-source-agreements",
    ]);
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.kill_on_drop(true);

    let output = match tokio::time::timeout(WINGET_QUERY_TIMEOUT, cmd.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => {
            return Err(CoreError::Internal(anyhow!(
                "Failed to run winget list for {}: {}",
                package_id,
                e
            )));
        }
        Err(_) => {
            return Err(CoreError::Internal(anyhow!(
                "winget list for {} timed out after {} seconds",
                package_id,
                WINGET_QUERY_TIMEOUT.as_secs()
            )));
        }
    };

    if !output.status.success() {
        // winget list returns non-zero when the package is not installed
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Output format:
    //   Name                         Id                               Version Available Source
    //   --------------------------------------------------------------------------------------
    //   Visual Studio Community 2026 Microsoft.VisualStudio.Community 18.9.1  18.10.0   winget
    //
    // Find the line containing the package_id and extract the version column.
    let mut past_separator = false;
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("---") {
            past_separator = true;
            continue;
        }
        if past_separator && trimmed.contains(package_id) {
            // Split on whitespace and find the token after the package ID
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if let Some(id_pos) = parts.iter().position(|&p| p == package_id) {
                if let Some(version) = parts.get(id_pos + 1) {
                    info!("{} is installed at version {}", package_id, version);
                    return Ok(Some(version.to_string()));
                }
            }
        }
    }

    Ok(None)
}

/// Query winget for available versions of a package, returned latest-first.
#[cfg(windows)]
async fn query_winget_versions(package_id: &str) -> Result<Vec<String>, CoreError> {
    use crate::repo::CREATE_NO_WINDOW;
    use tokio::process::Command;

    let mut cmd = Command::new("winget");
    cmd.args([
        "show",
        "--id",
        package_id,
        "--versions",
        "--accept-source-agreements",
    ]);
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.kill_on_drop(true);

    let output = match tokio::time::timeout(WINGET_QUERY_TIMEOUT, cmd.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => {
            return Err(CoreError::Internal(anyhow!(
                "Failed to run winget show --versions for {}: {}",
                package_id,
                e
            )));
        }
        Err(_) => {
            return Err(CoreError::Internal(anyhow!(
                "winget show --versions for {} timed out after {} seconds",
                package_id,
                WINGET_QUERY_TIMEOUT.as_secs()
            )));
        }
    };

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

#[cfg(windows)]
async fn install_build_tools_windows<T>(state: AppState<T>) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    use std::sync::atomic::Ordering;

    use crate::repo::CREATE_NO_WINDOW;
    use tokio::process::Command;

    if INSTALLING_BUILD_TOOLS
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err(CoreError::Internal(anyhow!(
            "Build tools installation is already in progress."
        )));
    }
    let _guard = InstallGuard;

    info!("Starting build tools installation");
    let _ = state
        .build_tools_tx
        .send("Starting build tools installation".to_string());

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

    let _ = state
        .build_tools_tx
        .send("Retrieving required tool versions...".to_string());

    let sdk_contents = tokio::fs::read_to_string(&sdk_json_path)
        .await
        .map_err(|e| {
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
    let mut reboot_required = false;

    // 1. Install .NET runtimes via winget.
    for dotnet_package in [
        "Microsoft.DotNet.DesktopRuntime.10",
        "Microsoft.DotNet.Runtime.10",
    ] {
        let _ = state
            .build_tools_tx
            .send(format!("Installing {}", dotnet_package));
        info!("Installing {}", dotnet_package);

        let mut cmd = Command::new("winget");
        cmd.args([
            "install",
            "--id",
            dotnet_package,
            "--silent",
            "--accept-source-agreements",
            "--accept-package-agreements",
        ]);
        cmd.creation_flags(CREATE_NO_WINDOW);

        match cmd.output().await {
            Ok(output) => {
                let exit_code = output.status.code();
                if output.status.success() {
                    info!("Successfully installed {}", dotnet_package);
                } else if exit_code == Some(EXIT_CODE_ALREADY_INSTALLED)
                    || exit_code == Some(EXIT_CODE_NO_UPGRADE_FOUND)
                {
                    info!("{} is already installed", dotnet_package);
                } else if exit_code == Some(EXIT_CODE_REBOOT_REQUIRED) {
                    info!("{} installed (reboot required)", dotnet_package);
                    reboot_required = true;
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    error!(
                        "{} install failed (exit {:?}): stdout={}, stderr={}",
                        dotnet_package, exit_code, stdout, stderr
                    );
                    errors.push(format!("{}: exit code {:?}", dotnet_package, exit_code));
                }
            }
            Err(e) => {
                error!("Failed to execute winget for {}: {}", dotnet_package, e);
                errors.push(format!("{}: {}", dotnet_package, e));
            }
        }
    }

    // 2. Install Visual Studio Community with components from the SDK config.
    //    Queries winget for available versions and picks the latest at or above
    //    MinimumVisualStudio2026Version. Combines all suggested components into
    //    the --override argument.
    {
        let _ = state
            .build_tools_tx
            .send("Installing Visual Studio Community (this may take 10-30 minutes)".to_string());
        let minimum = parse_version(&sdk.minimum_visual_studio_2026_version);

        // If VS is already installed at a version >= minimum, use that version
        // to avoid the "no applicable installer" error from winget.
        let installed = query_installed_version("Microsoft.VisualStudio.Community").await?;
        let installed_meets_minimum = installed
            .as_ref()
            .map(|v| parse_version(v) >= minimum)
            .unwrap_or(false);

        let vs_version: Option<String> = if installed_meets_minimum {
            info!(
                "Visual Studio Community {} is already installed (>= minimum {})",
                installed.as_ref().unwrap(),
                sdk.minimum_visual_studio_2026_version
            );
            installed
        } else {
            let vs_versions = query_winget_versions("Microsoft.VisualStudio.Community").await?;
            // Pick the latest version >= minimum (list is latest-first)
            vs_versions
                .into_iter()
                .find(|v| parse_version(v) >= minimum)
        };

        match vs_version {
            Some(version) => {
                let mut override_parts: Vec<String> =
                    vec!["--passive --wait --norestart --nocache".to_string()];

                for component in sdk.all_components() {
                    if !is_valid_component_id(component) {
                        warn!(
                            "Skipping component with unexpected characters: {:?}",
                            component
                        );
                        continue;
                    }
                    // Workload entries get ";includeRecommended", individual components do not
                    if component.contains("Workload.") {
                        override_parts.push(format!("--add {component};includeRecommended"));
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
                    "--accept-source-agreements",
                    "--accept-package-agreements",
                    "--version",
                    &version,
                    "--override",
                    &override_arg,
                ];

                info!("winget {}", args.join(" "));

                let mut cmd = Command::new("winget");
                cmd.args(args);
                cmd.creation_flags(CREATE_NO_WINDOW);

                match cmd.output().await {
                    Ok(output) => {
                        let exit_code = output.status.code();
                        if output.status.success() {
                            info!("Successfully installed Visual Studio Community");
                        } else if exit_code == Some(EXIT_CODE_REBOOT_REQUIRED) {
                            info!("Visual Studio Community installed (reboot required)");
                            reboot_required = true;
                        } else if exit_code == Some(EXIT_CODE_NO_APPLICABLE_INSTALLER) {
                            error!(
                                "Visual Studio installer was unable to run (exit {:?})",
                                exit_code
                            );
                            errors.push("Visual Studio: installer was unable to run. Ensure you allow the installer when prompted.".to_string());
                        } else {
                            let stderr = String::from_utf8_lossy(&output.stderr);
                            let stdout = String::from_utf8_lossy(&output.stdout);
                            error!(
                                "Visual Studio install failed (exit {:?}): stdout={}, stderr={}",
                                exit_code, stdout, stderr
                            );
                            errors.push(format!("Visual Studio: exit code {:?}", exit_code));
                        }
                    }
                    Err(e) => {
                        error!("Failed to execute winget for Visual Studio: {}", e);
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

    // 3. Install Visual C++ Redistributable from the engine's bundled installer.
    {
        let _ = state
            .build_tools_tx
            .send("Installing VC++ Redistributable".to_string());
        let vcredist_path = engine_path
            .join("Engine")
            .join("Extras")
            .join("Redist")
            .join("en-us")
            .join("vc_redist.x64.exe");

        if !vcredist_path.exists() {
            error!(
                "VC++ Redistributable installer not found at {}",
                vcredist_path.display()
            );
            errors.push(format!(
                "VC++ Redistributable: installer not found at {}. Ensure the engine is fully downloaded.",
                vcredist_path.display()
            ));
        } else {
            info!(
                "Installing VC++ Redistributable from {}",
                vcredist_path.display()
            );

            let mut cmd = Command::new(&vcredist_path);
            cmd.args(["/install", "/quiet", "/norestart"]);
            cmd.creation_flags(CREATE_NO_WINDOW);

            match cmd.output().await {
                Ok(output) => {
                    let exit_code = output.status.code();
                    if output.status.success() {
                        info!("Successfully installed VC++ Redistributable");
                    } else if exit_code == Some(EXIT_CODE_ALREADY_INSTALLED) {
                        info!("VC++ Redistributable is already installed");
                    } else if exit_code == Some(EXIT_CODE_REBOOT_REQUIRED) {
                        info!("VC++ Redistributable installed (reboot required)");
                        reboot_required = true;
                    } else {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        error!(
                            "VC++ Redistributable install failed (exit {:?}): stdout={}, stderr={}",
                            exit_code, stdout, stderr
                        );
                        errors.push(format!("VC++ Redistributable: exit code {:?}", exit_code));
                    }
                }
                Err(e) => {
                    error!("Failed to execute VC++ Redistributable installer: {}", e);
                    errors.push(format!("VC++ Redistributable: {}", e));
                }
            }
        }
    }

    if errors.is_empty() {
        if reboot_required {
            info!("All build tools installed successfully (reboot required to complete setup)");
        } else {
            info!("All build tools installed successfully");
        }
        Ok(())
    } else {
        Err(CoreError::Internal(anyhow!(
            "Some build tools failed to install:\n{}",
            errors.join("\n")
        )))
    }
}
