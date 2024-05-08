/// Custom serialization and deserialization config for [DateTime<Utc>] to
/// string having the format `%Y-%m-%dT%H:%M:%SZ`.
/// ```use chrono::{DateTime, Utc};
/// use crate::utils::serde::json_date_format;
/// use serde::{ Deserialize, Serialize };
///
/// #[derive(Serialize, Deserialize)]
/// struct Data {
///     #[serde(with = "json_date_format")]
///     date: DateTime<Utc>
/// }
/// ```
pub mod json_date_format {
    use chrono::{DateTime, Utc};
    use serde::{self, Deserialize, Deserializer, Serializer};

    const FORMAT: &str = "%Y-%m-%dT%H:%M:%SZ";

    pub fn serialize<S>(date: &DateTime<Utc>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let s = format!("{}", date.format(FORMAT));
        serializer.serialize_str(&s)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<DateTime<Utc>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse::<DateTime<Utc>>().map_err(serde::de::Error::custom)
    }
}
