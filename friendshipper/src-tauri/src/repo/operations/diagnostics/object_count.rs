use std::path::PathBuf;

use axum::extract::State;
use axum::Json;
use chrono::{DateTime, Utc};

use crate::engine::EngineProvider;
use ethos_core::clients::git;
use ethos_core::operations::GcOp;
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::ObjectCountResponse;
use ethos_core::worker::TaskSequence;

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
    let loose_count = parse_loose_count(&raw_output);
    let is_healthy = in_pack_count < HEALTHY_THRESHOLD_OBJECTS;
    let last_packed = last_packed_at(&git).await;

    Ok(Json(ObjectCountResponse {
        in_pack_count,
        is_healthy,
        raw_output,
        loose_count,
        last_packed,
    }))
}

pub async fn run_gc_handler<T>(State(state): State<AppState<T>>) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<CoreError>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);

    sequence.push(Box::new(GcOp {
        git_client: state.git(),
    }));

    let _ = state.operation_tx.send(sequence).await;
    if let Ok(Some(err)) = rx.await {
        return Err(err);
    }

    Ok(())
}

/// Read a `u64` out of a `git count-objects -v` body, e.g. `in-pack: 56789`.
///
/// Matches on an exact `<field>:` prefix rather than a substring: the fields include both `count`
/// and `size-pack`, so loose matching would let one field's value be read for another's.
/// Returns 0 when the field is absent or unparseable, matching the pre-existing behavior.
fn parse_count_field(output: &str, field: &str) -> u64 {
    let prefix = format!("{field}:");
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix(prefix.as_str()) {
            if let Ok(value) = rest.trim().parse::<u64>() {
                return value;
            }
        }
    }
    0
}

/// Objects in packfiles — repository size.
fn parse_in_pack(output: &str) -> u64 {
    parse_count_field(output, "in-pack")
}

/// Loose, unpacked objects — maintenance debt.
fn parse_loose_count(output: &str) -> u64 {
    parse_count_field(output, "count")
}

/// When the repository was last packed, taken from the commit-graph's mtime.
///
/// The commit-graph is used rather than the newest packfile mtime because it is the only one of the
/// two that fetching does not disturb: a fetch above `fetch.unpackLimit` writes a pack, and on a
/// large repo most fetches do. A full `commit-graph write --reachable` never no-ops, so the
/// timestamp moves on every maintenance run.
///
/// Deliberately reports maintenance by *any* agent — this app, the gc button below, or a
/// developer's own `git gc` — which is why the UI calls it "repo last packed" rather than claiming
/// it was a manual run.
///
/// `None` means never packed, which is the normal state of a fresh clone and must render as such
/// rather than as an error.
async fn last_packed_at(git: &git::Git) -> Option<DateTime<Utc>> {
    // Resolved by git rather than joined onto the repo path: in a worktree `.git` is a *file*, so
    // `<repo>/.git/objects/...` does not exist and a naive join would report "never" forever — for
    // precisely the developers this setting is aimed at.
    let resolved = git
        .run_and_collect_output(
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                "objects/info/commit-graph",
            ],
            git::Opts::new_without_logs(),
        )
        .await
        .ok()?;

    let path = PathBuf::from(resolved.trim());
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .map(DateTime::<Utc>::from)
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

    /// A realistic `count-objects -v` body has all five fields; each must read its own value.
    #[test]
    fn test_parse_loose_count() {
        let output = "count: 1234
size: 5678
in-pack: 56789
packs: 1
size-pack: 123456
";
        assert_eq!(parse_loose_count(output), 1234);
    }

    #[test]
    fn test_parse_loose_count_missing() {
        let output = "in-pack: 56789
packs: 1
";
        assert_eq!(parse_loose_count(output), 0);
    }

    /// The two fields must not read each other's values.
    ///
    /// This is a real hazard rather than a hypothetical: the body contains `count`, `in-pack`,
    /// `packs` and `size-pack`, so a substring or `split(':')`-based match can cross-talk. Distinct
    /// values here mean a regression shows up as a swap rather than as two identical numbers.
    #[test]
    fn test_count_fields_do_not_collide() {
        let output = "count: 11
size: 22
in-pack: 33
packs: 44
size-pack: 55
";
        assert_eq!(parse_loose_count(output), 11, "count read the wrong field");
        assert_eq!(parse_in_pack(output), 33, "in-pack read the wrong field");
    }

    /// `packs:` must not satisfy a lookup for `in-pack`, and vice versa.
    #[test]
    fn test_count_fields_require_exact_prefix() {
        assert_eq!(
            parse_count_field(
                "packs: 7
",
                "in-pack"
            ),
            0
        );
        assert_eq!(
            parse_count_field(
                "size-pack: 9
",
                "count"
            ),
            0
        );
        assert_eq!(
            parse_count_field(
                "count: 3
",
                "count"
            ),
            3
        );
    }

    fn run_git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn init_repo(dir: &std::path::Path) {
        for args in [
            vec!["init"],
            vec!["config", "user.email", "t@t.t"],
            vec!["config", "user.name", "t"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            run_git(dir, &args);
        }
        std::fs::write(dir.join("f.txt"), "seed").unwrap();
        run_git(dir, &["add", "f.txt"]);
        run_git(dir, &["commit", "-m", "seed"]);
    }

    fn git_client(dir: &std::path::Path) -> git::Git {
        let (tx, _rx) = std::sync::mpsc::channel();
        // the receiver is dropped, which is fine: every call here uses new_without_logs
        git::Git::new(dir.to_path_buf(), tx)
    }

    /// A freshly cloned repo has no commit-graph. That must read as "never packed", not as an
    /// error — it is the normal first-run state, not an edge case.
    #[tokio::test]
    async fn test_last_packed_is_none_without_commit_graph() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        assert!(
            last_packed_at(&git_client(dir.path())).await.is_none(),
            "a repo with no commit-graph must report None"
        );
    }

    #[tokio::test]
    async fn test_last_packed_is_some_after_commit_graph_write() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        run_git(dir.path(), &["commit-graph", "write", "--reachable"]);

        let packed = last_packed_at(&git_client(dir.path())).await;
        assert!(packed.is_some(), "commit-graph exists but was not detected");

        let age = chrono::Utc::now() - packed.unwrap();
        assert!(
            age.num_seconds().abs() < 300,
            "timestamp should be roughly now, got {packed:?}"
        );
    }

    /// The path must resolve when the repo is a **worktree**, where `.git` is a file rather than a
    /// directory and the object store lives in the main checkout's common dir.
    ///
    /// Joining `<repo>/.git/objects/info/commit-graph` silently fails in that layout, so this
    /// would have reported "never packed" forever — for exactly the developers running several
    /// worktrees that this whole setting exists to serve.
    #[tokio::test]
    async fn test_last_packed_resolves_through_a_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        init_repo(&main);
        run_git(&main, &["commit-graph", "write", "--reachable"]);

        let wt = dir.path().join("wt");
        run_git(
            &main,
            &["worktree", "add", wt.to_str().unwrap(), "-b", "side"],
        );
        assert!(
            wt.join(".git").is_file(),
            "precondition: a worktree's .git should be a file"
        );
        assert!(
            !wt.join(".git/objects/info/commit-graph").exists(),
            "precondition: the naive join must not resolve in a worktree"
        );

        assert!(
            last_packed_at(&git_client(&wt)).await.is_some(),
            "commit-graph must be found through a worktree's gitlink"
        );
    }

    #[test]
    fn test_parse_count_field_handles_garbage() {
        assert_eq!(
            parse_count_field(
                "count: not-a-number
",
                "count"
            ),
            0
        );
        assert_eq!(parse_count_field("", "count"), 0);
    }
}
