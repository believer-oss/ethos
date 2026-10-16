use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use kube_derive::CustomResource;
use lazy_static::lazy_static;
use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tracing::warn;

use crate::types::builds::{LaunchMode, LaunchOptions};
use crate::types::errors::CoreError;

pub const CLIENT_CAPTURE_ANNOTATION: &str = "believer.dev/client-capture";

lazy_static! {
    static ref CAPTURE_ARG_ALLOWLIST: Regex = Regex::new(
        r"(?i)^-(trace=[A-Za-z0-9_,.]+|tracefile=[A-Za-z0-9_-]+|tracefiletimestamps|statnamedevents)$"
    )
    .unwrap();
}

#[derive(Debug)]
pub struct GroupFullError;
impl IntoResponse for GroupFullError {
    fn into_response(self) -> Response {
        (StatusCode::BAD_REQUEST, "Error: Group is full").into_response()
    }
}

impl std::fmt::Display for GroupFullError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Group is full.")
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct Group {
    pub name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub users: Option<Vec<String>>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct LocalObjectReference {
    pub name: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct GroupStatus {
    pub name: String,

    #[serde(rename = "serverRef")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_ref: Option<LocalObjectReference>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub users: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready: Option<bool>,
}

#[derive(CustomResource, Default, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "game.believer.dev",
    version = "v1alpha1",
    kind = "Playtest",
    namespaced
)]
#[kube(status = "PlaytestStatus")]
pub struct PlaytestSpec {
    pub version: String,
    pub map: Option<String>,

    #[serde(rename = "displayName")]
    pub display_name: String,

    #[serde(rename = "minGroups")]
    pub min_groups: i32,

    #[serde(rename = "playersPerGroup")]
    pub players_per_group: i32,

    #[serde(rename = "startTime")]
    pub start_time: String,

    #[serde(rename = "feedbackURL")]
    pub feedback_url: String,

    #[serde(rename = "usersToAutoAssign")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub users_to_auto_assign: Option<Vec<String>>,

    #[serde(rename = "includeReadinessProbe")]
    pub include_readiness_probe: bool,

    #[serde(rename = "gameServerCmdArgs")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub game_server_cmd_args: Option<Vec<String>>,

    #[serde(
        rename = "gameClientCmdArgs",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub game_client_cmd_args: Option<Vec<String>>,

    #[serde(rename = "disableGameServers")]
    #[serde(default)]
    pub disable_game_servers: bool,

    pub groups: Vec<Group>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
pub struct PlaytestStatus {
    pub groups: Vec<GroupStatus>,
}

pub type GetPlaytestsResponse = Vec<Playtest>;

#[derive(Debug, Deserialize, Serialize)]
pub struct CreatePlaytestRequest {
    pub name: String,
    pub project: String,
    pub do_not_prune: bool,
    #[serde(default)]
    pub client_capture: bool,
    pub spec: PlaytestSpec,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UpdatePlaytestRequest {
    pub project: String,
    pub do_not_prune: bool,
    #[serde(default)]
    pub client_capture: bool,
    pub spec: PlaytestSpec,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AssignUserRequest {
    pub playtest: String,
    pub user: String,
    pub group: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UnassignUserRequest {
    pub playtest: String,
    pub user: String,
}

#[derive(Clone, Debug)]
pub struct PlaytestAssignment {
    pub server: String,
    pub version: String,
}

pub fn split_capture_args(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_owned).collect()
}

pub fn disallowed_capture_args(args: &[String]) -> Vec<String> {
    args.iter()
        .filter(|a| !CAPTURE_ARG_ALLOWLIST.is_match(a))
        .cloned()
        .collect()
}

pub fn validate_capture_request(
    client_capture: bool,
    args: &Option<Vec<String>>,
) -> Result<(), CoreError> {
    if !client_capture {
        return Ok(());
    }

    let detail = match args.as_deref() {
        None | Some([]) => Some("no arguments given".to_string()),
        Some(a) if a.iter().any(|t| t.trim().is_empty()) => Some("empty argument".to_string()),
        Some(a) => {
            let bad = disallowed_capture_args(a);
            (!bad.is_empty()).then(|| format!("disallowed: {}", bad.join(" ")))
        }
    };

    match detail {
        Some(d) => Err(CoreError::Input(anyhow::anyhow!(
            "Capture needs valid trace arguments: {d}"
        ))),
        None => Ok(()),
    }
}

pub fn capture_launch_args(playtest: &Playtest) -> Vec<String> {
    let Some(args) = playtest.spec.game_client_cmd_args.as_ref() else {
        return Vec::new();
    };

    args.iter()
        .filter(|a| {
            if a.trim().is_empty() {
                warn!("dropping empty client capture arg");
                false
            } else if !CAPTURE_ARG_ALLOWLIST.is_match(a) {
                warn!("dropping disallowed client capture arg: {a}");
                false
            } else {
                true
            }
        })
        .cloned()
        .collect()
}

impl Playtest {
    pub fn client_capture_enabled(&self) -> bool {
        self.metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get(CLIENT_CAPTURE_ANNOTATION))
            .is_some_and(|v| v == "true")
    }

    pub fn for_update(
        name: &str,
        existing: Playtest,
        input: UpdatePlaytestRequest,
        owner: String,
    ) -> Playtest {
        let mut playtest = Playtest::new(name, input.spec);
        playtest.metadata.resource_version = existing.metadata.resource_version;

        let mut annotations =
            BTreeMap::from([(String::from("believer.dev/project"), input.project)]);
        let existing_owner = existing
            .metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get("believer.dev/owner"))
            .cloned();
        annotations.insert(
            String::from("believer.dev/owner"),
            existing_owner.unwrap_or(owner),
        );
        if input.do_not_prune {
            annotations.insert(
                String::from("believer.dev/do-not-prune"),
                "true".to_string(),
            );
        }
        if input.client_capture {
            annotations.insert(CLIENT_CAPTURE_ANNOTATION.to_string(), "true".to_string());
        }

        playtest.metadata.annotations = Some(annotations);
        playtest.spec.groups = existing.spec.groups;
        playtest
    }
}

pub fn find_capture_playtest<'a>(
    playtests: &'a [Playtest],
    launch: &LaunchOptions,
) -> Option<&'a Playtest> {
    match launch.launch_mode {
        LaunchMode::WithServer => {
            if launch.name.is_empty() {
                return None;
            }
            playtests.iter().find(|p| {
                p.status.as_ref().is_some_and(|s| {
                    s.groups
                        .iter()
                        .any(|g| g.server_ref.as_ref().is_some_and(|r| r.name == launch.name))
                })
            })
        }
        LaunchMode::WithoutServer => {
            let name = launch.playtest.as_ref()?;
            playtests
                .iter()
                .find(|p| p.metadata.name.as_deref() == Some(name.as_str()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::api::ObjectMeta;

    fn playtest(name: &str) -> Playtest {
        let mut p = Playtest::new(name, PlaytestSpec::default());
        p.metadata = ObjectMeta {
            name: Some(name.to_string()),
            ..Default::default()
        };
        p
    }

    fn with_annotations(mut p: Playtest, pairs: &[(&str, &str)]) -> Playtest {
        p.metadata.annotations = Some(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        );
        p
    }

    fn with_server(mut p: Playtest, server: Option<&str>) -> Playtest {
        p.status = Some(PlaytestStatus {
            groups: vec![GroupStatus {
                name: "g".to_string(),
                server_ref: server.map(|n| LocalObjectReference {
                    name: n.to_string(),
                }),
                users: None,
                ready: None,
            }],
        });
        p
    }

    fn update_request(client_capture: bool, do_not_prune: bool) -> UpdatePlaytestRequest {
        UpdatePlaytestRequest {
            project: "proj".to_string(),
            do_not_prune,
            client_capture,
            spec: PlaytestSpec {
                game_client_cmd_args: Some(vec!["-trace=cpu".to_string()]),
                ..Default::default()
            },
        }
    }

    fn launch(mode: LaunchMode, name: &str, playtest: Option<&str>) -> LaunchOptions {
        LaunchOptions {
            name: name.to_string(),
            launch_mode: mode,
            playtest: playtest.map(str::to_owned),
        }
    }

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    fn spec_json() -> serde_json::Value {
        serde_json::json!({
            "version": "v",
            "map": null,
            "displayName": "d",
            "minGroups": 1,
            "playersPerGroup": 1,
            "startTime": "t",
            "feedbackURL": "f",
            "includeReadinessProbe": false,
            "groups": []
        })
    }

    #[test]
    fn absent_client_args_deserialize_to_none() {
        let spec: PlaytestSpec = serde_json::from_value(spec_json()).unwrap();
        assert!(spec.game_client_cmd_args.is_none());
    }

    #[test]
    fn none_client_args_omit_the_key() {
        let spec: PlaytestSpec = serde_json::from_value(spec_json()).unwrap();
        let v = serde_json::to_value(&spec).unwrap();
        assert!(v.get("gameClientCmdArgs").is_none());
    }

    #[test]
    fn unknown_spec_fields_are_ignored() {
        let mut v = spec_json();
        v["somethingNew"] = serde_json::json!(1);
        assert!(serde_json::from_value::<PlaytestSpec>(v).is_ok());
    }

    #[test]
    fn round_trip_keeps_client_args_and_annotation() {
        let mut p = with_annotations(playtest("pt"), &[(CLIENT_CAPTURE_ANNOTATION, "true")]);
        p.spec.game_client_cmd_args = Some(strings(&["-trace=cpu", "-statnamedevents"]));

        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(
            v["spec"]["gameClientCmdArgs"],
            serde_json::json!(["-trace=cpu", "-statnamedevents"])
        );
        assert_eq!(
            v["metadata"]["annotations"][CLIENT_CAPTURE_ANNOTATION],
            "true"
        );

        let back: Playtest = serde_json::from_value(v).unwrap();
        assert_eq!(
            back.spec.game_client_cmd_args,
            Some(strings(&["-trace=cpu", "-statnamedevents"]))
        );
        assert!(back.client_capture_enabled());
    }

    #[test]
    fn for_update_sets_capture_annotation_only_when_requested() {
        let existing = playtest("pt");
        let on = Playtest::for_update(
            "pt",
            existing.clone(),
            update_request(true, false),
            "me".into(),
        );
        assert!(on.client_capture_enabled());

        let off = Playtest::for_update("pt", existing, update_request(false, false), "me".into());
        assert!(!off.client_capture_enabled());
    }

    #[test]
    fn for_update_does_not_carry_stale_capture_annotation() {
        let existing = with_annotations(
            playtest("pt"),
            &[
                (CLIENT_CAPTURE_ANNOTATION, "true"),
                ("believer.dev/do-not-prune", "true"),
            ],
        );
        let out = Playtest::for_update("pt", existing, update_request(false, false), "me".into());
        let a = out.metadata.annotations.unwrap();
        assert!(!a.contains_key(CLIENT_CAPTURE_ANNOTATION));
        assert!(!a.contains_key("believer.dev/do-not-prune"));
    }

    #[test]
    fn for_update_owner_prefers_existing() {
        let existing = with_annotations(playtest("pt"), &[("believer.dev/owner", "orig")]);
        let out = Playtest::for_update(
            "pt",
            existing,
            update_request(false, false),
            "caller".into(),
        );
        assert_eq!(
            out.metadata.annotations.unwrap()["believer.dev/owner"],
            "orig"
        );

        let out = Playtest::for_update(
            "pt",
            playtest("pt"),
            update_request(false, false),
            "caller".into(),
        );
        assert_eq!(
            out.metadata.annotations.unwrap()["believer.dev/owner"],
            "caller"
        );
    }

    #[test]
    fn for_update_do_not_prune_only_when_set() {
        let out = Playtest::for_update(
            "pt",
            playtest("pt"),
            update_request(false, true),
            "me".into(),
        );
        let a = out.metadata.annotations.unwrap();
        assert_eq!(a["believer.dev/do-not-prune"], "true");
        assert_eq!(a["believer.dev/project"], "proj");
    }

    #[test]
    fn for_update_takes_groups_version_from_existing_and_args_from_request() {
        let mut existing = playtest("pt");
        existing.metadata.resource_version = Some("42".to_string());
        existing.spec.groups = vec![Group {
            name: "g1".to_string(),
            users: None,
        }];
        existing.spec.game_client_cmd_args = Some(strings(&["-statnamedevents"]));

        let out = Playtest::for_update("pt", existing, update_request(true, false), "me".into());
        assert_eq!(out.metadata.resource_version.as_deref(), Some("42"));
        assert_eq!(out.spec.groups.len(), 1);
        assert_eq!(out.spec.groups[0].name, "g1");
        assert_eq!(
            out.spec.game_client_cmd_args,
            Some(strings(&["-trace=cpu"]))
        );
    }

    #[test]
    fn find_capture_with_server_matches_server_ref() {
        let pts = vec![
            with_server(playtest("a"), Some("other")),
            with_server(playtest("b"), Some("srv")),
        ];
        let found = find_capture_playtest(&pts, &launch(LaunchMode::WithServer, "srv", None));
        assert_eq!(found.unwrap().metadata.name.as_deref(), Some("b"));
    }

    #[test]
    fn find_capture_with_server_misses() {
        let l = launch(LaunchMode::WithServer, "srv", None);
        assert!(find_capture_playtest(&[playtest("a")], &l).is_none());
        assert!(find_capture_playtest(&[with_server(playtest("a"), None)], &l).is_none());
        assert!(find_capture_playtest(&[with_server(playtest("a"), Some("x"))], &l).is_none());
        let empty = launch(LaunchMode::WithServer, "", None);
        assert!(find_capture_playtest(&[with_server(playtest("a"), Some(""))], &empty).is_none());
    }

    #[test]
    fn find_capture_without_server_matches_playtest_not_name() {
        let pts = vec![playtest("pt")];
        let hit = launch(LaunchMode::WithoutServer, "pt", Some("pt"));
        assert!(find_capture_playtest(&pts, &hit).is_some());

        let name_only = launch(LaunchMode::WithoutServer, "pt", None);
        assert!(find_capture_playtest(&pts, &name_only).is_none());

        let wrong = launch(LaunchMode::WithoutServer, "pt", Some("zzz"));
        assert!(find_capture_playtest(&pts, &wrong).is_none());
    }

    #[test]
    fn validate_capture_request_rejects_invalid_when_enabled() {
        for args in [
            None,
            Some(vec![]),
            Some(strings(&[""])),
            Some(strings(&["-ExecCmds=x"])),
            Some(strings(&["-trace=cpu", "-ExecCmds=x"])),
        ] {
            assert!(validate_capture_request(true, &args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn validate_capture_request_accepts_valid_args() {
        let args = Some(strings(&[
            "-trace=default",
            "-tracefile=FellowshipTrace",
            "-tracefiletimestamps",
            "-statnamedevents",
        ]));
        assert!(validate_capture_request(true, &args).is_ok());
    }

    #[test]
    fn validate_capture_request_ignores_args_when_disabled() {
        for args in [
            None,
            Some(vec![]),
            Some(strings(&[""])),
            Some(strings(&["-ExecCmds=x"])),
        ] {
            assert!(validate_capture_request(false, &args).is_ok());
        }
    }

    #[test]
    fn allowlist_accepts_capture_args() {
        let args = strings(&[
            "-trace=default",
            "-tracefile=FellowshipTrace",
            "-tracefiletimestamps",
            "-statnamedevents",
            "-trace=cpu,gpu,frame.x",
            "-TRACE=cpu",
        ]);
        assert!(disallowed_capture_args(&args).is_empty());
    }

    #[test]
    fn allowlist_rejects_unsafe_args() {
        let args = strings(&[
            "-ExecCmds=x",
            "-tracefile=C:\\x",
            "-tracefile=../x",
            "-trace=",
            "-statnamedevents=1",
            "trace=cpu",
        ]);
        assert_eq!(disallowed_capture_args(&args), args);
    }

    #[test]
    fn capture_launch_args_filters_disallowed_and_empty() {
        let mut p = playtest("pt");
        p.spec.game_client_cmd_args = Some(strings(&[
            "-trace=default",
            "",
            "  ",
            "-ExecCmds=x",
            "-statnamedevents",
        ]));
        assert_eq!(
            capture_launch_args(&p),
            strings(&["-trace=default", "-statnamedevents"])
        );
        assert!(capture_launch_args(&playtest("none")).is_empty());
    }

    #[test]
    fn split_capture_args_splits_on_whitespace() {
        assert_eq!(
            split_capture_args("  -a   -b\t-c "),
            strings(&["-a", "-b", "-c"])
        );
        assert!(split_capture_args("   ").is_empty());
    }
}
