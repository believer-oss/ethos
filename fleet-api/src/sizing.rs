//! Fleet sizing: the Agones Buffer policy a (map, cluster) Fleet's autoscaler runs with.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use kube::KubeSchema;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// Agones' Buffer policy rules (`ValidateBufferPolicy` in `fleetautoscaler.go`, 1.55), on the fields a
// sizing object sets. Each starts with the parked guard (`parked: true` makes the other fields
// ignored) and checks only the pairs the object has, so partial objects pass; `Sizing::validate`
// checks the resolved value in full.
//
// A percentage `bufferSize` is a string (the schema allows only `N%` strings), an absolute one an
// integer. The CRD tells them apart with `type()`, which costs nothing to estimate; any test on the
// string itself (`string(…)`) is unbounded for an int-or-string, and the apiserver refuses rules whose
// estimated cost, times the list sizes, exceeds its budget. The local CEL evaluator used by the
// unit tests has no `type()`, so they use the equivalent string test.
#[cfg(not(test))]
macro_rules! is_percent {
    () => {
        "type(self.bufferSize) == string"
    };
}
#[cfg(test)]
macro_rules! is_percent {
    () => {
        "string(self.bufferSize).endsWith('%')"
    };
}

pub(crate) const MIN_LE_MAX_RULE: &str = "(has(self.parked) && self.parked) || !has(self.minReplicas) || !has(self.maxReplicas) || self.minReplicas <= self.maxReplicas";
pub(crate) const ABSOLUTE_LE_MAX_RULE: &str = concat!(
    "(has(self.parked) && self.parked) || !has(self.bufferSize) || ",
    is_percent!(),
    " || !has(self.maxReplicas) || int(self.bufferSize) <= self.maxReplicas"
);
pub(crate) const ABSOLUTE_MIN_RULE: &str = concat!(
    "(has(self.parked) && self.parked) || !has(self.bufferSize) || ",
    is_percent!(),
    " || !has(self.minReplicas) || self.minReplicas == 0 || self.minReplicas >= int(self.bufferSize)"
);
pub(crate) const PERCENT_MIN_RULE: &str = concat!(
    "(has(self.parked) && self.parked) || !has(self.bufferSize) || !(",
    is_percent!(),
    ") || !has(self.minReplicas) || self.minReplicas >= 1"
);
/// `Shard.spec.warm`: the buffer must be absolute.
pub(crate) const ABSOLUTE_BUFFER_RULE: &str =
    concat!("!has(self.bufferSize) || !(", is_percent!(), ")");

/// The sizing of one (map, cluster) Fleet, or a partial one to be filled from a less specific level.
///
/// The same shape is used everywhere: `Shard.spec.warm` and `live`, `FleetRelease.spec.overrides`
/// and, resolved, `LocalFleetRelease.spec.maps[].sizing`. An unset field is filled from the next
/// less specific level ([`Sizing::filled_from`]); which levels apply, and in what order, is the
/// coordinator's. `parked: true` parks the Fleet (no autoscaler, zero replicas apart from Allocated
/// servers) and the other fields are then ignored.
///
/// CEL refuses a sizing object that breaks Agones' Buffer rules on the fields it sets:
/// - `minReplicas ≤ maxReplicas`;
/// - absolute `bufferSize` (an integer): `1 ≤ bufferSize ≤ maxReplicas`, and `minReplicas` is `0` or
///   at least `bufferSize`;
/// - percentage `bufferSize` (`"1%"`–`"99%"`): `minReplicas ≥ 1`.
///
/// The shard's `limits.maxReplicasPerFleet` lives in another object, so CEL can't check it;
/// [`Sizing::validate`] does, on the resolved value.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, KubeSchema)]
#[serde(rename_all = "camelCase")]
#[x_kube(validation = Rule::new(MIN_LE_MAX_RULE).message("minReplicas must not exceed maxReplicas"))]
#[x_kube(validation = Rule::new(ABSOLUTE_LE_MAX_RULE).message("an absolute bufferSize must not exceed maxReplicas"))]
#[x_kube(validation = Rule::new(ABSOLUTE_MIN_RULE).message("with an absolute bufferSize, minReplicas must be 0 or at least bufferSize"))]
#[x_kube(validation = Rule::new(PERCENT_MIN_RULE).message("a percentage bufferSize needs minReplicas of at least 1"))]
#[cfg_attr(test, x_kube(cel))]
pub struct Sizing {
    /// Ready servers to keep on top of Allocated ones: an integer, or a percentage of the Fleet
    /// such as `"20%"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buffer_size: Option<BufferSize>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0))]
    pub min_replicas: Option<i32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub max_replicas: Option<i32>,

    /// Parks the Fleet: scaled to zero (Agones spares Allocated servers) with no autoscaler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parked: Option<bool>,
}

impl Sizing {
    /// A parked sizing: `{parked: true}`.
    pub fn parked() -> Self {
        Sizing {
            parked: Some(true),
            ..Default::default()
        }
    }

    /// An absolute Buffer policy with every field set.
    pub fn absolute(buffer_size: i32, min_replicas: i32, max_replicas: i32) -> Self {
        Sizing {
            buffer_size: Some(BufferSize::Absolute(buffer_size)),
            min_replicas: Some(min_replicas),
            max_replicas: Some(max_replicas),
            parked: None,
        }
    }

    /// Whether this sizing parks the Fleet.
    pub fn is_parked(&self) -> bool {
        self.parked == Some(true)
    }

    /// This sizing with every unset field taken from `base`, the next less specific level.
    pub fn filled_from(&self, base: &Sizing) -> Sizing {
        Sizing {
            buffer_size: self.buffer_size.or(base.buffer_size),
            min_replicas: self.min_replicas.or(base.min_replicas),
            max_replicas: self.max_replicas.or(base.max_replicas),
            parked: self.parked.or(base.parked),
        }
    }

    /// Checks a resolved sizing: Agones' Buffer rules on the complete value, plus the shard's
    /// `limits.maxReplicasPerFleet` if it has one. A parked sizing is always valid. An unset
    /// `minReplicas` counts as `0`, as in Agones; `bufferSize` and `maxReplicas` are required.
    ///
    /// Returns every problem, not just the first, so a form can mark each field.
    pub fn validate(&self, max_replicas_per_fleet: Option<i32>) -> Result<(), Vec<SizingError>> {
        if self.is_parked() {
            return Ok(());
        }
        let mut errors = Vec::new();
        let min = self.min_replicas.unwrap_or(0);
        if min < 0 {
            errors.push(SizingError::NegativeMinReplicas);
        }
        match self.max_replicas {
            None => errors.push(SizingError::MissingMaxReplicas),
            Some(max) => {
                if min > max {
                    errors.push(SizingError::MinAboveMax);
                }
                if let Some(limit) = max_replicas_per_fleet.filter(|&limit| max > limit) {
                    errors.push(SizingError::AboveLimit { limit });
                }
            }
        }
        match self.buffer_size {
            None => errors.push(SizingError::MissingBufferSize),
            Some(BufferSize::Absolute(buffer)) => {
                if buffer < 1 {
                    errors.push(SizingError::BufferBelowOne);
                }
                if self.max_replicas.is_some_and(|max| max < buffer) {
                    errors.push(SizingError::BufferAboveMax);
                }
                if min != 0 && min < buffer {
                    errors.push(SizingError::MinBelowBuffer);
                }
            }
            Some(BufferSize::Percent(percent)) => {
                if !(1..=99).contains(&percent) {
                    errors.push(SizingError::PercentOutOfRange);
                }
                if min < 1 {
                    errors.push(SizingError::PercentNeedsMinReplicas);
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// Why a resolved [`Sizing`] is invalid. The messages name the fields as they appear in YAML.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SizingError {
    #[error("bufferSize is required")]
    MissingBufferSize,
    #[error("maxReplicas is required")]
    MissingMaxReplicas,
    #[error("minReplicas must not be negative")]
    NegativeMinReplicas,
    #[error("minReplicas must not exceed maxReplicas")]
    MinAboveMax,
    #[error("an absolute bufferSize must be at least 1")]
    BufferBelowOne,
    #[error("an absolute bufferSize must not exceed maxReplicas")]
    BufferAboveMax,
    #[error("with an absolute bufferSize, minReplicas must be 0 or at least bufferSize")]
    MinBelowBuffer,
    #[error("a percentage bufferSize must be between 1% and 99%")]
    PercentOutOfRange,
    #[error("a percentage bufferSize needs minReplicas of at least 1")]
    PercentNeedsMinReplicas,
    #[error("maxReplicas must not exceed the shard's limit of {limit} per fleet")]
    AboveLimit { limit: i32 },
}

impl SizingError {
    /// The sizing field the error is about, as named in YAML.
    pub fn field(&self) -> &'static str {
        match self {
            SizingError::MissingBufferSize
            | SizingError::BufferBelowOne
            | SizingError::PercentOutOfRange => "bufferSize",
            SizingError::MissingMaxReplicas
            | SizingError::BufferAboveMax
            | SizingError::AboveLimit { .. } => "maxReplicas",
            SizingError::NegativeMinReplicas
            | SizingError::MinAboveMax
            | SizingError::MinBelowBuffer
            | SizingError::PercentNeedsMinReplicas => "minReplicas",
        }
    }
}

/// A Buffer policy's `bufferSize`: an integer, or a percentage written `"N%"`.
///
/// In YAML it is an int-or-string, as in Agones: `2` or `"20%"`. The schema refuses any other
/// string, as Agones does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BufferSize {
    Absolute(i32),
    Percent(u8),
}

impl fmt::Display for BufferSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BufferSize::Absolute(n) => write!(f, "{n}"),
            BufferSize::Percent(p) => write!(f, "{p}%"),
        }
    }
}

/// A `bufferSize` string that isn't `N%`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid bufferSize {0:?}: expected an integer or a percentage such as \"20%\"")]
pub struct ParseBufferSizeError(String);

impl FromStr for BufferSize {
    type Err = ParseBufferSizeError;

    /// Parses `"2"` or `"20%"`, e.g. from a form field.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || ParseBufferSizeError(s.to_string());
        let (digits, percent) = match s.strip_suffix('%') {
            Some(digits) => (digits, true),
            None => (s, false),
        };
        // `parse` alone would accept a leading `+`.
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }
        if percent {
            digits
                .parse()
                .map(BufferSize::Percent)
                .map_err(|_| invalid())
        } else {
            digits
                .parse()
                .map(BufferSize::Absolute)
                .map_err(|_| invalid())
        }
    }
}

impl Serialize for BufferSize {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            BufferSize::Absolute(n) => serializer.serialize_i32(*n),
            BufferSize::Percent(_) => serializer.collect_str(self),
        }
    }
}

impl<'de> Deserialize<'de> for BufferSize {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Int(i32),
            Str(String),
        }
        match Raw::deserialize(deserializer)? {
            Raw::Int(n) => Ok(BufferSize::Absolute(n)),
            Raw::Str(s) if s.ends_with('%') => s.parse().map_err(serde::de::Error::custom),
            Raw::Str(s) => Err(serde::de::Error::custom(ParseBufferSizeError(s))),
        }
    }
}

impl JsonSchema for BufferSize {
    fn schema_name() -> Cow<'static, str> {
        "BufferSize".into()
    }

    fn inline_schema() -> bool {
        true
    }

    /// An int-or-string, narrowed as Agones narrows it. JSON Schema applies `minimum` to numbers
    /// only and `pattern` to strings only, so one schema bounds both forms: an integer of at least
    /// 1, or `1%`–`99%`.
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "x-kubernetes-int-or-string": true,
            "minimum": 1,
            "pattern": "^[1-9][0-9]?%$",
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn cel_ok(value: serde_json::Value) {
        if let Err(errors) = Sizing::validate_cel(&value, None) {
            panic!("{value} should pass: {errors}");
        }
    }

    fn cel_err(value: serde_json::Value, message: &str) {
        let errors = Sizing::validate_cel(&value, None).expect_err(&format!("{value} should fail"));
        assert!(
            errors
                .as_slice()
                .iter()
                .any(|e| e.to_string().contains(message)),
            "{value}: expected {message:?}, got {errors}"
        );
    }

    #[test]
    fn buffer_size_round_trips() {
        let sizing: Sizing = serde_json::from_value(json!({"bufferSize": 2})).unwrap();
        assert_eq!(sizing.buffer_size, Some(BufferSize::Absolute(2)));
        assert_eq!(
            serde_json::to_value(&sizing).unwrap(),
            json!({"bufferSize": 2})
        );

        let sizing: Sizing = serde_json::from_value(json!({"bufferSize": "20%"})).unwrap();
        assert_eq!(sizing.buffer_size, Some(BufferSize::Percent(20)));
        assert_eq!(
            serde_json::to_value(&sizing).unwrap(),
            json!({"bufferSize": "20%"})
        );

        for bad in [
            json!("20"),
            json!("x"),
            json!("%"),
            json!("1.5%"),
            json!(" 2%"),
            json!("-1%"),
            json!(2.5),
        ] {
            assert!(
                serde_json::from_value::<Sizing>(json!({"bufferSize": bad})).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn buffer_size_parses_form_input() {
        assert_eq!("3".parse(), Ok(BufferSize::Absolute(3)));
        assert_eq!("15%".parse(), Ok(BufferSize::Percent(15)));
        assert!("".parse::<BufferSize>().is_err());
        assert!("abc".parse::<BufferSize>().is_err());
        assert!("300%".parse::<BufferSize>().is_err());
        assert_eq!(BufferSize::Percent(15).to_string(), "15%");
    }

    #[test]
    fn empty_and_partial_sizings_pass_cel() {
        cel_ok(json!({}));
        cel_ok(json!({"bufferSize": 2}));
        cel_ok(json!({"bufferSize": "20%"}));
        cel_ok(json!({"minReplicas": 2, "maxReplicas": 20}));
        cel_ok(json!({"bufferSize": "20%", "minReplicas": 1, "maxReplicas": 10}));
        cel_ok(json!({"bufferSize": 2, "minReplicas": 0, "maxReplicas": 50}));
        cel_ok(json!({"bufferSize": 2, "minReplicas": 2, "maxReplicas": 2}));
    }

    #[test]
    fn cel_enforces_agones_buffer_rules() {
        cel_err(
            json!({"minReplicas": 3, "maxReplicas": 2}),
            "minReplicas must not exceed",
        );
        cel_err(
            json!({"bufferSize": 5, "maxReplicas": 4}),
            "must not exceed maxReplicas",
        );
        cel_err(
            json!({"bufferSize": 2, "minReplicas": 1}),
            "0 or at least bufferSize",
        );
        cel_err(json!({"bufferSize": "20%", "minReplicas": 0}), "at least 1");
        // The absolute rules don't apply to percentages, nor the percentage rule to integers.
        cel_ok(json!({"bufferSize": "50%", "minReplicas": 1, "maxReplicas": 1}));
        cel_ok(json!({"bufferSize": 1, "minReplicas": 0}));
    }

    #[test]
    fn parked_sizing_skips_the_rules() {
        cel_ok(json!({"parked": true}));
        cel_ok(json!({"parked": true, "minReplicas": 3, "maxReplicas": 2}));
        cel_err(
            json!({"parked": false, "minReplicas": 3, "maxReplicas": 2}),
            "minReplicas",
        );
    }

    #[test]
    fn filled_from_takes_unset_fields_only() {
        let default = Sizing {
            buffer_size: Some(BufferSize::Percent(20)),
            min_replicas: Some(1),
            max_replicas: Some(10),
            parked: None,
        };
        let map = Sizing {
            min_replicas: Some(2),
            max_replicas: Some(20),
            ..Default::default()
        };
        let resolved = map.filled_from(&default);
        assert_eq!(resolved.buffer_size, Some(BufferSize::Percent(20)));
        assert_eq!(resolved.min_replicas, Some(2));
        assert_eq!(resolved.max_replicas, Some(20));
        assert!(!resolved.is_parked());

        // A parked level stays parked unless a more specific one says `parked: false`.
        let parked = Sizing::parked().filled_from(&default);
        assert!(parked.is_parked());
        let unparked = Sizing {
            parked: Some(false),
            ..Default::default()
        };
        assert!(!unparked.filled_from(&Sizing::parked()).is_parked());
        assert!(Sizing::default().filled_from(&Sizing::parked()).is_parked());
    }

    #[test]
    fn validate_checks_the_resolved_value() {
        assert_eq!(Sizing::absolute(2, 0, 50).validate(Some(50)), Ok(()));
        assert_eq!(Sizing::parked().validate(Some(1)), Ok(()));
        let live = Sizing {
            buffer_size: Some(BufferSize::Percent(20)),
            min_replicas: Some(1),
            max_replicas: Some(10),
            parked: None,
        };
        assert_eq!(live.validate(None), Ok(()));

        assert_eq!(
            Sizing::default().validate(None),
            Err(vec![
                SizingError::MissingMaxReplicas,
                SizingError::MissingBufferSize
            ])
        );
        assert_eq!(
            Sizing::absolute(2, 0, 60).validate(Some(50)),
            Err(vec![SizingError::AboveLimit { limit: 50 }])
        );
        assert_eq!(
            Sizing::absolute(0, 0, 5).validate(None),
            Err(vec![SizingError::BufferBelowOne])
        );
        assert_eq!(
            Sizing::absolute(3, 2, 2).validate(None),
            Err(vec![
                SizingError::BufferAboveMax,
                SizingError::MinBelowBuffer
            ])
        );
        assert_eq!(
            Sizing::absolute(1, 5, 4).validate(None),
            Err(vec![SizingError::MinAboveMax])
        );
        let percent_without_min = Sizing {
            min_replicas: None,
            ..live.clone()
        };
        assert_eq!(
            percent_without_min.validate(None),
            Err(vec![SizingError::PercentNeedsMinReplicas])
        );
        let percent_too_big = Sizing {
            buffer_size: Some(BufferSize::Percent(100)),
            ..live
        };
        assert_eq!(
            percent_too_big.validate(None),
            Err(vec![SizingError::PercentOutOfRange])
        );
        assert_eq!(SizingError::AboveLimit { limit: 1 }.field(), "maxReplicas");
    }
}
