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
