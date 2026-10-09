//! `clientCaptureUpload` dynamic config block.

use serde::{Deserialize, Deserializer, Serialize};
use tracing::warn;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureWatchEntry {
    pub dir: String,
    pub patterns: Vec<String>,
    pub key_prefix: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ClientCaptureUploadConfig {
    pub watch: Vec<CaptureWatchEntry>,
    pub quiescence_seconds: u64,
    pub bucket: Option<String>,
    pub pause_while_game_running: bool,
    pub max_upload_mbps: Option<f64>,
    pub keep_uploaded_traces: u32,
    pub ledger_retention_days: u32,
    pub part_size_mib: u64,
    pub part_concurrency: u32,
}

impl Default for ClientCaptureUploadConfig {
    fn default() -> Self {
        Self {
            watch: Vec::new(),
            quiescence_seconds: 30,
            bucket: None,
            pause_while_game_running: true,
            max_upload_mbps: None,
            keep_uploaded_traces: 5,
            ledger_retention_days: 30,
            part_size_mib: 32,
            part_concurrency: 4,
        }
    }
}

impl ClientCaptureUploadConfig {
    pub fn sanitized(mut self) -> Self {
        self.quiescence_seconds = self.quiescence_seconds.clamp(5, 3600);
        self.max_upload_mbps = self.max_upload_mbps.filter(|x| x.is_finite() && *x >= 0.1);
        self.keep_uploaded_traces = self.keep_uploaded_traces.min(100);
        self.ledger_retention_days = self.ledger_retention_days.clamp(1, 365);
        self.part_size_mib = self.part_size_mib.clamp(8, 256);
        self.part_concurrency = self.part_concurrency.clamp(1, 8);
        self.bucket = self.bucket.filter(|b| !b.is_empty());
        self
    }
}

/// Deserializes leniently so a malformed block never fails the whole dynamic config load.
pub fn lenient_capture_config<'de, D>(d: D) -> Result<Option<ClientCaptureUploadConfig>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(d)?;
    if value.is_null() {
        return Ok(None);
    }
    match serde_json::from_value::<ClientCaptureUploadConfig>(value) {
        Ok(cfg) => Ok(Some(cfg.sanitized())),
        Err(e) => {
            warn!("Ignoring invalid clientCaptureUpload: {e}");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::config::DynamicConfig;

    fn parse(json: &str) -> ClientCaptureUploadConfig {
        serde_json::from_str::<ClientCaptureUploadConfig>(json)
            .unwrap()
            .sanitized()
    }

    #[test]
    fn empty_object_yields_defaults() {
        let cfg = parse("{}");
        assert_eq!(cfg, ClientCaptureUploadConfig::default());
        assert!(cfg.watch.is_empty());
        assert_eq!(cfg.quiescence_seconds, 30);
        assert!(cfg.pause_while_game_running);
        assert_eq!(cfg.keep_uploaded_traces, 5);
        assert_eq!(cfg.ledger_retention_days, 30);
        assert_eq!(cfg.part_size_mib, 32);
        assert_eq!(cfg.part_concurrency, 4);
    }

    #[test]
    fn bad_type_is_ignored_and_rest_of_config_loads() {
        let dc: DynamicConfig = serde_json::from_str(
            r#"{"profileDataPath":"abc","playtestRegions":["us"],"clientCaptureUpload":{"watch":5}}"#,
        )
        .unwrap();
        assert!(dc.client_capture_upload.is_none());
        assert_eq!(dc.profile_data_path, "abc");
        assert_eq!(dc.playtest_regions, vec!["us".to_string()]);
    }

    #[test]
    fn null_and_missing_yield_none() {
        let dc: DynamicConfig = serde_json::from_str(r#"{"clientCaptureUpload":null}"#).unwrap();
        assert!(dc.client_capture_upload.is_none());
        let dc: DynamicConfig = serde_json::from_str("{}").unwrap();
        assert!(dc.client_capture_upload.is_none());
    }

    #[test]
    fn valid_block_in_dynamic_config_is_parsed_and_clamped() {
        let dc: DynamicConfig = serde_json::from_str(
            r#"{"clientCaptureUpload":{"watch":[{"dir":"a","patterns":["*.log"],"keyPrefix":"p"}],"partConcurrency":99}}"#,
        )
        .unwrap();
        let cfg = dc.client_capture_upload.unwrap();
        assert_eq!(cfg.watch.len(), 1);
        assert_eq!(cfg.part_concurrency, 8);
    }

    #[test]
    fn clamp_boundaries() {
        assert_eq!(parse(r#"{"quiescenceSeconds":4}"#).quiescence_seconds, 5);
        assert_eq!(parse(r#"{"quiescenceSeconds":5}"#).quiescence_seconds, 5);
        assert_eq!(
            parse(r#"{"quiescenceSeconds":3600}"#).quiescence_seconds,
            3600
        );
        assert_eq!(
            parse(r#"{"quiescenceSeconds":3601}"#).quiescence_seconds,
            3600
        );
        assert_eq!(parse(r#"{"keepUploadedTraces":0}"#).keep_uploaded_traces, 0);
        assert_eq!(
            parse(r#"{"keepUploadedTraces":100}"#).keep_uploaded_traces,
            100
        );
        assert_eq!(
            parse(r#"{"keepUploadedTraces":101}"#).keep_uploaded_traces,
            100
        );
        assert_eq!(
            parse(r#"{"ledgerRetentionDays":0}"#).ledger_retention_days,
            1
        );
        assert_eq!(
            parse(r#"{"ledgerRetentionDays":1}"#).ledger_retention_days,
            1
        );
        assert_eq!(
            parse(r#"{"ledgerRetentionDays":365}"#).ledger_retention_days,
            365
        );
        assert_eq!(
            parse(r#"{"ledgerRetentionDays":366}"#).ledger_retention_days,
            365
        );
        assert_eq!(parse(r#"{"partSizeMib":7}"#).part_size_mib, 8);
        assert_eq!(parse(r#"{"partSizeMib":8}"#).part_size_mib, 8);
        assert_eq!(parse(r#"{"partSizeMib":256}"#).part_size_mib, 256);
        assert_eq!(parse(r#"{"partSizeMib":257}"#).part_size_mib, 256);
        assert_eq!(parse(r#"{"partConcurrency":0}"#).part_concurrency, 1);
        assert_eq!(parse(r#"{"partConcurrency":1}"#).part_concurrency, 1);
        assert_eq!(parse(r#"{"partConcurrency":8}"#).part_concurrency, 8);
        assert_eq!(parse(r#"{"partConcurrency":9}"#).part_concurrency, 8);
    }

    #[test]
    fn max_upload_mbps_rules() {
        assert_eq!(parse(r#"{"maxUploadMbps":0.05}"#).max_upload_mbps, None);
        assert_eq!(parse(r#"{"maxUploadMbps":0.1}"#).max_upload_mbps, Some(0.1));
        assert_eq!(
            parse(r#"{"maxUploadMbps":50.0}"#).max_upload_mbps,
            Some(50.0)
        );
        for bad in [f64::NAN, f64::INFINITY] {
            let cfg = ClientCaptureUploadConfig {
                max_upload_mbps: Some(bad),
                ..Default::default()
            };
            assert_eq!(cfg.sanitized().max_upload_mbps, None);
        }
    }

    #[test]
    fn empty_bucket_is_none() {
        assert_eq!(parse(r#"{"bucket":""}"#).bucket, None);
        assert_eq!(parse(r#"{"bucket":"b"}"#).bucket.as_deref(), Some("b"));
    }
}
