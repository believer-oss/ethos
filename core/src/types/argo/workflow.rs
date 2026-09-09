use kube_derive::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(CustomResource, Default, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "argoproj.io",
    version = "v1alpha1",
    kind = "Workflow",
    namespaced
)]
#[kube(status = "WorkflowStatus")]
pub struct WorkflowSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<WorkflowArguments>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        rename = "workflowTemplateRef"
    )]
    pub workflow_template_ref: Option<WorkflowTemplateRef>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowArguments {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Vec<WorkflowParameter>>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
pub struct WorkflowParameter {
    pub name: String,
    pub value: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
pub struct WorkflowTemplateRef {
    pub name: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
pub struct CreatePromoteBuildWorkflowRequest {
    pub commit: String,
    /// Backend environment for the deploy. Serde name stays `shard`: it is the
    /// Argo template's parameter name and the wire field shared with the server.
    #[serde(rename = "shard")]
    pub backend_environment: Option<String>,
    pub metadata_path: Option<String>,
    pub pusher: Option<String>,
    pub distribution: Option<String>,
    pub steam_branch: Option<String>,
    pub game_config: Option<String>,
}

impl CreatePromoteBuildWorkflowRequest {
    /// Assemble the Argo workflow parameters for this promotion request.
    ///
    /// `backend_environment` is deliberately three-state and the distinction is
    /// load-bearing: `None` omits the parameter entirely so the workflow
    /// template's own default applies, `Some("")` emits it empty to suppress the
    /// backend deploy, and `Some(v)` emits the configured environment. Never
    /// normalize `None` into `Some("")`.
    pub fn to_workflow_parameters(&self) -> Vec<WorkflowParameter> {
        let mut params = vec![
            WorkflowParameter {
                name: "commit".to_string(),
                value: self.commit.clone(),
            },
            WorkflowParameter {
                name: "game_config".to_string(),
                // An empty configured value falls back to the default rather than
                // submitting an empty parameter.
                value: self
                    .game_config
                    .clone()
                    .filter(|config| !config.is_empty())
                    .unwrap_or_else(|| "development".to_string()),
            },
        ];

        if let Some(metadata_path) = &self.metadata_path {
            params.push(WorkflowParameter {
                name: "metadata_path".to_string(),
                value: metadata_path.clone(),
            });
        }
        if let Some(backend_environment) = &self.backend_environment {
            params.push(WorkflowParameter {
                // Argo template parameter name; not renamed with the Rust field.
                name: "shard".to_string(),
                value: backend_environment.clone(),
            });
        }
        if let Some(distribution) = &self.distribution {
            params.push(WorkflowParameter {
                name: "distribution".to_string(),
                value: distribution.clone(),
            });
        }
        if let Some(steam_branch) = &self.steam_branch {
            params.push(WorkflowParameter {
                name: "steam_branch".to_string(),
                value: steam_branch.clone(),
            });
        }

        params
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowStatus {
    pub phase: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub estimated_duration: Option<u64>,
    pub progress: Option<String>,
    pub nodes: Option<HashMap<String, WorkflowNodeStatus>>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowNodeStatus {
    pub id: String,
    pub name: String,
    pub display_name: String,
    #[serde(rename = "type")]
    pub node_type: Option<String>,
    pub phase: String,
    pub started_at: Option<String>,
    pub template_name: Option<String>,
    pub template_ref: Option<TemplateRef>,
    pub outputs: Option<Outputs>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TemplateRef {
    pub name: Option<String>,
    pub template: Option<String>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Outputs {
    pub artifacts: Option<Vec<Artifact>>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub s3: Option<S3Artifact>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
pub struct S3Artifact {
    pub key: String,
}

#[cfg(test)]
mod tests {
    //! Fixtures 1 and 2 cover promotions the UI previously could not express: a
    //! launcher promotion with no backend deploy, and a Steam branch promotion.
    //! Fixtures 3-5 guard existing behavior.

    use super::*;

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn request() -> CreatePromoteBuildWorkflowRequest {
        CreatePromoteBuildWorkflowRequest {
            commit: SHA.to_string(),
            ..Default::default()
        }
    }

    fn find<'a>(params: &'a [WorkflowParameter], name: &str) -> Option<&'a WorkflowParameter> {
        params.iter().find(|p| p.name == name)
    }

    fn value<'a>(params: &'a [WorkflowParameter], name: &str) -> &'a str {
        find(params, name)
            .unwrap_or_else(|| panic!("expected parameter {name} to be present"))
            .value
            .as_str()
    }

    /// Launcher promotion with the backend deploy suppressed.
    #[test]
    fn launcher_with_backend_deploy_off() {
        let params = CreatePromoteBuildWorkflowRequest {
            metadata_path: Some("meta/path-one".to_string()),
            backend_environment: Some(String::new()),
            ..request()
        }
        .to_workflow_parameters();

        assert_eq!(value(&params, "commit"), SHA);
        assert_eq!(value(&params, "game_config"), "development");
        assert_eq!(value(&params, "metadata_path"), "meta/path-one");
        assert_eq!(value(&params, "shard"), "");
        assert!(find(&params, "distribution").is_none());
        assert!(find(&params, "steam_branch").is_none());
    }

    /// Steam branch promotion.
    #[test]
    fn steam_branch_promotion() {
        let params = CreatePromoteBuildWorkflowRequest {
            backend_environment: Some(String::new()),
            distribution: Some("steam".to_string()),
            steam_branch: Some("branch-one".to_string()),
            game_config: Some("Test".to_string()),
            ..request()
        }
        .to_workflow_parameters();

        assert_eq!(value(&params, "commit"), SHA);
        // Capitalized deliberately - Steam rejects "Development".
        assert_eq!(value(&params, "game_config"), "Test");
        assert_eq!(value(&params, "distribution"), "steam");
        assert_eq!(value(&params, "steam_branch"), "branch-one");
        assert_eq!(value(&params, "shard"), "");
        assert!(find(&params, "metadata_path").is_none());
    }

    /// Today's launcher behavior; must not change.
    #[test]
    fn launcher_with_backend_deploy_on() {
        let params = CreatePromoteBuildWorkflowRequest {
            metadata_path: Some("meta/path-one".to_string()),
            backend_environment: Some("backend-env-one".to_string()),
            ..request()
        }
        .to_workflow_parameters();

        assert_eq!(value(&params, "game_config"), "development");
        assert_eq!(value(&params, "metadata_path"), "meta/path-one");
        assert_eq!(value(&params, "shard"), "backend-env-one");
        assert!(find(&params, "distribution").is_none());
        assert!(find(&params, "steam_branch").is_none());
    }

    /// A destination with no backend environment relies on the Argo template
    /// default. Emitting an empty value would silently suppress it, so absence is
    /// correct - do not "fix" this test to expect an empty string.
    #[test]
    fn backend_deploy_on_with_no_configured_environment_omits_the_parameter() {
        let params = request().to_workflow_parameters();

        assert_eq!(value(&params, "commit"), SHA);
        assert_eq!(value(&params, "game_config"), "development");
        assert!(find(&params, "shard").is_none());
        assert!(find(&params, "metadata_path").is_none());
        assert!(find(&params, "distribution").is_none());
        assert!(find(&params, "steam_branch").is_none());
        assert_eq!(params.len(), 2);
    }

    /// Suppressing a template-default backend deploy. Differs from the fixture
    /// above only in backend_environment being Some("") rather than None, and must
    /// produce observably different output.
    #[test]
    fn backend_deploy_off_with_no_configured_environment_emits_empty() {
        let params = CreatePromoteBuildWorkflowRequest {
            backend_environment: Some(String::new()),
            ..request()
        }
        .to_workflow_parameters();

        assert_eq!(value(&params, "game_config"), "development");
        assert_eq!(value(&params, "shard"), "");
        assert!(find(&params, "metadata_path").is_none());
    }
}
