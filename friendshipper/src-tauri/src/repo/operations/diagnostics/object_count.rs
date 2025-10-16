use axum::extract::State;
use axum::Json;

use crate::engine::EngineProvider;
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::ObjectCountResponse;

use crate::state::AppState;

const HEALTHY_THRESHOLD_OBJECTS: u64 = 25_000_000; // 25 million objects

pub async fn object_count_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<ObjectCountResponse>, CoreError>
where
    T: EngineProvider,
{
    let git = state.git();
    let raw_output = git.count_objects().await?;

    // Parse the output to extract in-pack value (number of objects in packfiles)
    // Example output:
    // count: 1234
    // size: 5678
    // in-pack: 56789  <- number of objects in packfiles, not size
    // packs: 1
    // size-pack: 123456
    let in_pack_count = parse_in_pack(&raw_output);
    let is_healthy = in_pack_count < HEALTHY_THRESHOLD_OBJECTS;

    Ok(Json(ObjectCountResponse {
        in_pack_count,
        is_healthy,
        raw_output,
    }))
}

fn parse_in_pack(output: &str) -> u64 {
    for line in output.lines() {
        if line.starts_with("in-pack:") {
            if let Some(value_str) = line.split(':').nth(1) {
                if let Ok(value) = value_str.trim().parse::<u64>() {
                    return value;
                }
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_in_pack() {
        let output = "count: 1234\nsize: 5678\nin-pack: 56789\npacks: 1\nsize-pack: 123456\n";
        assert_eq!(parse_in_pack(output), 56789);
    }

    #[test]
    fn test_parse_in_pack_missing() {
        let output = "count: 1234\nsize: 5678\n";
        assert_eq!(parse_in_pack(output), 0);
    }

    #[test]
    fn test_threshold() {
        assert_eq!(HEALTHY_THRESHOLD_OBJECTS, 25_000_000);
    }
}
