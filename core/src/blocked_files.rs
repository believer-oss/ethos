use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};

/// Compiles and matches `blockedFileGlobs` patterns from `friendshipper.yaml`
/// against repo-relative, forward-slash-separated paths.
///
/// This module is a pure matching primitive: it does no logging itself
/// (callers log `invalid`/`warnings` — the config-load path is the only one
/// that does, since the on-demand compile used by `StatusOp` runs on every
/// status refresh and would otherwise spam the log continuously), and it has
/// no opinion about `FileState::Deleted`. A file whose state is `Deleted`
/// must never be blocked — removing a wrongly-committed asset is the
/// cleanup path this feature exists to preserve — so callers must filter out
/// deleted files before calling `is_blocked`.
pub struct BlockedFileMatcher(GlobSet);

/// Result of compiling a project's `blockedFileGlobs` list.
pub struct BlockedFileMatcherResult {
    pub matcher: BlockedFileMatcher,
    /// (pattern, error message) for patterns that failed to compile.
    pub invalid: Vec<(String, String)>,
    /// (pattern, warning message) for patterns that will silently under-match.
    pub warnings: Vec<(String, String)>,
}

impl BlockedFileMatcher {
    /// Compile `patterns` into a matcher. Never fails: invalid patterns are
    /// collected in `invalid` and excluded from the matcher.
    ///
    /// Named `compile` rather than `new` because it returns
    /// `BlockedFileMatcherResult` — bundling the matcher with per-pattern
    /// diagnostics — rather than `Self`.
    pub fn compile(patterns: &[String]) -> BlockedFileMatcherResult {
        let mut builder = GlobSetBuilder::new();
        let mut invalid: Vec<(String, String)> = Vec::new();
        let mut warnings: Vec<(String, String)> = Vec::new();

        for pattern in patterns {
            let glob: Result<Glob, _> = GlobBuilder::new(pattern)
                .case_insensitive(true)
                .literal_separator(true)
                .backslash_escape(false)
                .build();

            match glob {
                Ok(glob) => {
                    if pattern.contains('\\') {
                        // A Windows-style pattern like `Content\Characters\**`
                        // compiles without error and doesn't trip the
                        // under-match check below (it does contain `**`), but
                        // it is platform-inconsistent in a way that is easy to
                        // miss: globset's own parser normalizes a `\` in the
                        // *pattern* to `/` whenever `std::path::is_separator`
                        // considers `\` a separator on the compiling platform
                        // — true on Windows, false on Unix — regardless of
                        // this module's own `backslash_escape(false)` setting,
                        // which only controls escape semantics, not this
                        // separator normalization. So on Windows the pattern
                        // is silently rewritten to `Content/Characters/**`
                        // and happens to match correctly; on Linux/macOS `\`
                        // stays a literal character, and since a real
                        // repo-relative path is always forward-slash (per
                        // this module's contract), the pattern matches
                        // nothing there. A project author who authors and
                        // tests `friendshipper.yaml` on Windows would never
                        // see this fail — only their Linux/macOS teammates'
                        // submits would silently go unprotected. Warn
                        // unconditionally on any backslash in the pattern
                        // rather than rely on that platform difference.
                        warnings.push((
                            pattern.clone(),
                            format!(
                                "pattern `{pattern}` contains a backslash (`\\`); \
                                 blockedFileGlobs patterns must use forward slashes (`/`) as the \
                                 path separator. Whether a backslash acts as a directory \
                                 separator here depends on the platform compiling this pattern, \
                                 so the same friendshipper.yaml can protect assets on Windows \
                                 while silently protecting nothing on Linux/macOS. Did you mean \
                                 `{}`?",
                                pattern.replace('\\', "/")
                            ),
                        ));
                    }

                    if !pattern.contains('/') && !pattern.contains("**") {
                        warnings.push((
                            pattern.clone(),
                            format!(
                                "pattern `{pattern}` contains neither `/` nor `**`; because \
                                 matching uses literal_separator semantics, this will only match \
                                 a file named exactly `{pattern}` at the repository root, not in \
                                 any subdirectory. This likely does not do what you intend — did \
                                 you mean `**/{pattern}`?"
                            ),
                        ));
                    }
                    builder.add(glob);
                }
                Err(e) => {
                    invalid.push((pattern.clone(), e.to_string()));
                }
            }
        }

        let matcher = match builder.build() {
            Ok(set) => BlockedFileMatcher(set),
            Err(e) => {
                // Building a GlobSet from already-individually-valid Globs is
                // not expected to fail in practice, but GlobSetBuilder::build
                // is fallible, and this function's contract is "never fails,
                // never panics" — so degrade to an empty matcher rather than
                // unwrap. This drops every pattern that made it past the loop
                // above, which is the fail-safe direction: a bad or missing
                // block on submitted files is a lesser failure than bricking
                // app startup for every user of the project.
                invalid.push((
                    "<all patterns>".to_string(),
                    format!("failed to build combined glob set: {e}"),
                ));
                BlockedFileMatcher::empty()
            }
        };

        BlockedFileMatcherResult {
            matcher,
            invalid,
            warnings,
        }
    }

    /// A matcher that matches nothing.
    pub fn empty() -> Self {
        BlockedFileMatcher(GlobSet::empty())
    }

    /// `path` must be repo-relative and forward-slash separated.
    pub fn is_blocked(&self, path: &str) -> bool {
        self.0.is_match(path)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patterns(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    // -- Literal, *, **, ?, {a,b}, and [a-z] patterns -----------------------

    #[test]
    fn literal_pattern_matches_exact_path_only() {
        let result = BlockedFileMatcher::compile(&patterns(&["Content/Foo.uasset"]));
        assert!(result.matcher.is_blocked("Content/Foo.uasset"));
        assert!(!result.matcher.is_blocked("Content/Bar.uasset"));
        assert!(!result.matcher.is_blocked("Content/Sub/Foo.uasset"));
    }

    #[test]
    fn star_pattern_matches_within_one_directory_level() {
        let result = BlockedFileMatcher::compile(&patterns(&["Content/*.uasset"]));
        assert!(result.matcher.is_blocked("Content/Foo.uasset"));
        assert!(!result.matcher.is_blocked("Content/Sub/Foo.uasset"));
    }

    #[test]
    fn double_star_pattern_matches_at_any_depth() {
        let result = BlockedFileMatcher::compile(&patterns(&["Content/**/*.uasset"]));
        assert!(result.matcher.is_blocked("Content/Foo.uasset"));
        assert!(result.matcher.is_blocked("Content/Sub/Foo.uasset"));
        assert!(result.matcher.is_blocked("Content/Sub/Deeper/Foo.uasset"));
    }

    #[test]
    fn question_mark_pattern_matches_single_character() {
        let result = BlockedFileMatcher::compile(&patterns(&["Content/Foo?.uasset"]));
        assert!(result.matcher.is_blocked("Content/Foo1.uasset"));
        assert!(!result.matcher.is_blocked("Content/Foo12.uasset"));
        assert!(!result.matcher.is_blocked("Content/Foo.uasset"));
    }

    #[test]
    fn alternate_pattern_matches_either_branch() {
        let result = BlockedFileMatcher::compile(&patterns(&["Content/**/*.{uasset,umap}"]));
        assert!(result.matcher.is_blocked("Content/Foo.uasset"));
        assert!(result.matcher.is_blocked("Content/Sub/Level.umap"));
        assert!(!result.matcher.is_blocked("Content/Foo.txt"));
    }

    #[test]
    fn character_class_pattern_matches_range() {
        let result = BlockedFileMatcher::compile(&patterns(&["Content/[A-C]*.uasset"]));
        assert!(result.matcher.is_blocked("Content/Apple.uasset"));
        assert!(result.matcher.is_blocked("Content/Banana.uasset"));
        assert!(!result.matcher.is_blocked("Content/Zebra.uasset"));
    }

    // -- literal_separator behavior (the semantics this module most depends
    //    on getting right: without it, `*` and `?` would cross directory
    //    boundaries, and a pattern like `Content/*.uasset` would incorrectly
    //    match `Content/Sub/Foo.uasset`) --------------------------

    #[test]
    fn single_star_does_not_cross_directory_boundary() {
        let result = BlockedFileMatcher::compile(&patterns(&["Content/*.uasset"]));
        assert!(
            !result.matcher.is_blocked("Content/Sub/Foo.uasset"),
            "Content/*.uasset must not match a nested path under literal_separator(true); \
             globset's default (literal_separator(false)) would incorrectly match here"
        );
    }

    #[test]
    fn double_star_does_cross_directory_boundary() {
        let result = BlockedFileMatcher::compile(&patterns(&["Content/**/*.uasset"]));
        assert!(
            result.matcher.is_blocked("Content/Sub/Foo.uasset"),
            "Content/**/*.uasset must match a nested path — ** is the escape hatch from \
             literal_separator's directory-boundary restriction"
        );
    }

    // -- Case-insensitivity, both directions --------------------------------

    #[test]
    fn matching_is_case_insensitive_pattern_upper_path_lower() {
        let result = BlockedFileMatcher::compile(&patterns(&["**/*.UASSET"]));
        assert!(result.matcher.is_blocked("Content/foo.uasset"));
    }

    #[test]
    fn matching_is_case_insensitive_pattern_lower_path_upper() {
        let result = BlockedFileMatcher::compile(&patterns(&["**/*.uasset"]));
        assert!(result.matcher.is_blocked("Content/FOO.UASSET"));
    }

    // -- Empty / absent pattern lists ---------------------------------------

    #[test]
    fn empty_pattern_list_matches_nothing() {
        let result = BlockedFileMatcher::compile(&[]);
        assert!(result.matcher.is_empty());
        assert!(!result.matcher.is_blocked("Content/Foo.uasset"));
        assert!(!result.matcher.is_blocked(""));
        assert!(result.invalid.is_empty());
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn empty_matcher_matches_nothing() {
        let matcher = BlockedFileMatcher::empty();
        assert!(matcher.is_empty());
        assert!(!matcher.is_blocked("Content/Foo.uasset"));
    }

    // -- Multiple / overlapping patterns -------------------------------------

    #[test]
    fn multiple_patterns_each_contribute_matches() {
        let result = BlockedFileMatcher::compile(&patterns(&["**/*.uasset", "**/*.umap"]));
        assert!(result.matcher.is_blocked("Content/Foo.uasset"));
        assert!(result.matcher.is_blocked("Content/Level.umap"));
        assert!(!result.matcher.is_blocked("Content/Readme.txt"));
    }

    #[test]
    fn overlapping_patterns_matching_the_same_path_are_not_an_error() {
        let result = BlockedFileMatcher::compile(&patterns(&["**/*.uasset", "Content/**"]));
        assert!(result.invalid.is_empty());
        // Both patterns independently match this path; is_blocked only
        // reports true/false, not which pattern(s) fired.
        assert!(result.matcher.is_blocked("Content/Foo.uasset"));
    }

    // -- Invalid patterns are skipped, valid siblings still match -----------

    #[test]
    fn invalid_pattern_is_skipped_and_reported_valid_sibling_still_matches() {
        let result =
            BlockedFileMatcher::compile(&patterns(&["Content/[unterminated", "**/*.uasset"]));
        assert_eq!(result.invalid.len(), 1);
        assert_eq!(result.invalid[0].0, "Content/[unterminated");
        assert!(
            !result.invalid[0].1.is_empty(),
            "invalid entry must carry a non-empty error message"
        );
        assert!(result.matcher.is_blocked("Content/Foo.uasset"));
    }

    #[test]
    fn all_invalid_patterns_yields_empty_matcher_not_an_error() {
        // Both patterns are unambiguously malformed: an unterminated
        // character class and an unterminated brace-alternate group.
        let result = BlockedFileMatcher::compile(&patterns(&["Content/[abc", "Content/{a,b"]));
        assert_eq!(result.invalid.len(), 2);
        assert!(result.matcher.is_empty());
    }

    // -- Under-match warning heuristic ---------------------------------------

    #[test]
    fn warning_fires_for_pattern_with_no_slash_and_no_double_star() {
        let result = BlockedFileMatcher::compile(&patterns(&["*.uasset"]));
        assert_eq!(result.warnings.len(), 1);
        assert_eq!(result.warnings[0].0, "*.uasset");
        // The pattern is still compiled and used as-authored, not rewritten —
        // the warning informs, it doesn't silently correct the user's intent.
        assert!(result.matcher.is_blocked("Foo.uasset"));
        assert!(!result.matcher.is_blocked("Content/Foo.uasset"));
    }

    #[test]
    fn warning_does_not_fire_for_double_star_pattern() {
        let result = BlockedFileMatcher::compile(&patterns(&["**/*.uasset"]));
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn warning_does_not_fire_for_pattern_containing_a_literal_slash() {
        let result = BlockedFileMatcher::compile(&patterns(&["Content/*.uasset"]));
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn warning_does_not_fire_for_an_invalid_pattern() {
        // An invalid pattern is already reported via `invalid`; it should not
        // also generate a redundant/confusing under-match warning.
        let result = BlockedFileMatcher::compile(&patterns(&["[unterminated"]));
        assert_eq!(result.invalid.len(), 1);
        assert!(result.warnings.is_empty());
    }

    // -- Spaces and non-ASCII paths ------------------------------------------

    #[test]
    fn matches_path_with_spaces() {
        let result = BlockedFileMatcher::compile(&patterns(&["**/*.uasset"]));
        assert!(result
            .matcher
            .is_blocked("Content/Environments/Foo Bar/Wall.uasset"));
    }

    #[test]
    fn matches_path_with_non_ascii_characters() {
        let result = BlockedFileMatcher::compile(&patterns(&["**/*.uasset"]));
        assert!(result.matcher.is_blocked("Content/Café/Décor/Table.uasset"));
        assert!(result
            .matcher
            .is_blocked("Content/キャラクター/Hero.uasset"));
    }

    #[test]
    fn pattern_itself_may_contain_non_ascii_characters() {
        let result = BlockedFileMatcher::compile(&patterns(&["Content/Café/**"]));
        assert!(result.matcher.is_blocked("Content/Café/Décor/Table.uasset"));
        assert!(!result.matcher.is_blocked("Content/Other/Table.uasset"));
    }

    // -- Forward-slash assumption, pinned explicitly -------------------------

    #[test]
    fn matching_operates_on_forward_slash_separated_paths() {
        // This module's documented contract (see the struct doc comment) is
        // that `path` is repo-relative and forward-slash separated, matching
        // exactly what `git status --porcelain` yields. This test pins that a
        // pattern written with `/` reliably matches a `/`-separated candidate
        // path on whichever platform the test suite runs on — this file has
        // no #[cfg(windows)]/#[cfg(unix)] branches precisely because CI runs
        // this same suite on both `build-linux` and `build-windows`
        // (.github/workflows/rust.yml), and both must agree.
        let result = BlockedFileMatcher::compile(&patterns(&["Content/Sub/*.uasset"]));
        assert!(result.matcher.is_blocked("Content/Sub/Foo.uasset"));
    }

    #[test]
    fn a_path_using_backslash_separators_is_platform_dependent() {
        // Callers must never hand this module a Windows-style backslash path
        // (git never produces one; RepoStatus::File::path is always
        // forward-slash) — but this pins the *actual*, verified behavior
        // rather than assuming it is uniform across platforms.
        //
        // `GlobSet::is_match` takes `P: AsRef<Path>` and routes through
        // `globset`'s internal `Candidate::new`, which normalizes the
        // candidate via `pathutil::normalize_path`. That function is a
        // documented no-op on Unix (verified in globset 0.4.19's
        // `src/pathutil.rs`), so on Unix a literal `\` is just another
        // character, not a separator, and does not multi-segment the path.
        // On non-Unix platforms (Windows), the same function rewrites every
        // byte for which `std::path::is_separator` holds — which includes
        // `\` — to `/` before matching. So on Windows a backslash-separated
        // candidate *is* normalized and does become multi-segment, purely
        // as a side effect of globset's internal candidate preparation, not
        // this module's own logic. That behavior is implementation-verified,
        // not a documented API contract, so it is pinned here with a test
        // that runs on both CI platforms (see .github/workflows/rust.yml's
        // build-linux and build-windows jobs) rather than merely assumed.
        let result = BlockedFileMatcher::compile(&patterns(&["Content/Sub/*.uasset"]));
        #[cfg(unix)]
        assert!(!result.matcher.is_blocked("Content\\Sub\\Foo.uasset"));
        #[cfg(not(unix))]
        assert!(result.matcher.is_blocked("Content\\Sub\\Foo.uasset"));
    }

    // -- Deletion handling is explicitly NOT this module's job ---------------
    //
    // A deleted file must never be blocked — removing a wrongly-committed
    // asset from a branch is the exact cleanup workflow this feature exists
    // to preserve, and blocking it would leave manual git surgery as the only
    // remedy. But that check depends on `FileState`, which this module has no
    // notion of: `is_blocked` takes only a path. There is deliberately no
    // `FileState` parameter here and therefore no test for it in this file —
    // coverage for "deletions matching a blocked glob are not blocked" lives
    // instead against the real `update_files_submit_status` call site in
    // `friendshipper/src-tauri/src/repo/operations/status.rs`, since that is
    // where the `FileState` check actually lives (see
    // `deleted_blocked_path_is_not_blocked` there).

    // -- Backslash patterns (Windows-style separators) -----------------------

    #[test]
    fn warning_fires_for_pattern_containing_backslash() {
        let result = BlockedFileMatcher::compile(&patterns(&["Content\\Characters\\**"]));
        assert!(
            result.invalid.is_empty(),
            "a backslash pattern still compiles: {:?}",
            result.invalid
        );
        assert_eq!(
            result.warnings.len(),
            1,
            "expected exactly one warning: {:?}",
            result.warnings
        );
        assert_eq!(result.warnings[0].0, "Content\\Characters\\**");
        assert!(result.warnings[0].1.contains('\\'));

        // Whether the pattern *also* matches anything is platform-dependent
        // (see a_path_using_backslash_separators_is_platform_dependent above
        // for the same underlying reason) and is not the point of this
        // warning: globset's parser normalizes `\` to `/` in the pattern
        // whenever std::path::is_separator considers `\` a separator on the
        // compiling platform, which is true on Windows and false on Unix.
        // So on Windows this pattern is silently rewritten to
        // `Content/Characters/**` and matches by accident; on Unix `\` stays
        // literal and it matches nothing — the failure mode a Windows author
        // testing locally would never observe.
        #[cfg(unix)]
        assert!(!result.matcher.is_blocked("Content/Characters/Hero.uasset"));
        #[cfg(not(unix))]
        assert!(result.matcher.is_blocked("Content/Characters/Hero.uasset"));
    }

    #[test]
    fn warning_does_not_fire_for_pattern_without_backslash() {
        let result = BlockedFileMatcher::compile(&patterns(&["Content/**/*.uasset"]));
        assert!(result.warnings.is_empty());
    }
}
