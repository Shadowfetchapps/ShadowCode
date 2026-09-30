//! Heads-ups about what a finished task changed that can make it look more
//! done than it is: tests skipped, deleted or weakened, CI and hook
//! configuration changed, checks switched off in the code, snapshots
//! rewritten.
//!
//! Only this task's own additions count (lines added, files deleted or
//! changed by the task, compared with the file before the task), and the
//! checks are plain text rules, never a model. The same rules apply to
//! ShadowCode's own agent and to subscription CLIs, because both are read
//! from the task's recorded changes.
use crate::{store::Store, workspace::Workspace};
use serde::Serialize;
use serde_json::{json, Value};

/// Most files looked at for one task.
const MAX_FILES: usize = 400;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Flag {
    /// `skipped_tests`, `deleted_tests`, `removed_tests`,
    /// `removed_assertions`, `ci_changed`, `checks_disabled`, `snapshots`.
    pub kind: &'static str,
    pub path: String,
    /// The first line concerned in the file after the task, when known.
    pub line: Option<usize>,
    /// What was found, in words.
    pub text: String,
}

/// A test file by its path.
pub fn is_test_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    lower.starts_with("tests/")
        || lower.starts_with("test/")
        || lower.contains("/tests/")
        || lower.contains("/test/")
        || lower.contains("__tests__/")
        || lower.contains("/spec/")
        || lower.starts_with("spec/")
        || lower.starts_with("e2e/")
        || lower.contains("/e2e/")
        || name.starts_with("test_")
        || name.contains("_test.")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name.ends_with("_spec.rb")
        || name.ends_with("test.java")
        || name.ends_with("tests.cs")
}

fn is_ci_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.starts_with(".github/workflows/")
        || lower.starts_with(".github/actions/")
        || lower == ".gitlab-ci.yml"
        || lower.starts_with(".circleci/")
        || lower == "jenkinsfile"
        || lower == "azure-pipelines.yml"
        || lower == ".travis.yml"
        || lower == "bitbucket-pipelines.yml"
        || lower == ".pre-commit-config.yaml"
        || lower.starts_with(".husky/")
        || lower == "lefthook.yml"
        || lower == ".buildkite/pipeline.yml"
}

fn is_snapshot(path: &str) -> bool {
    path.contains("__snapshots__/") || path.ends_with(".snap") || path.ends_with(".snap.new")
}

/// Markers that skip or focus tests (`it.only` skips every other test).
const SKIP_MARKERS: &[&str] = &[
    "#[ignore",
    "it.skip(",
    "test.skip(",
    "describe.skip(",
    "it.only(",
    "test.only(",
    "describe.only(",
    "xit(",
    "xtest(",
    "xdescribe(",
    "@pytest.mark.skip",
    "@pytest.mark.xfail",
    "pytest.skip(",
    "@unittest.skip",
    "self.skipTest(",
    "t.Skip(",
    "@Disabled",
    "@Ignore",
    "[Ignore]",
    "[Fact(Skip",
];

/// Byte `at` of `line` starts a token: it does not continue a name.
fn token_start(line: &str, at: usize) -> bool {
    !line[..at]
        .chars()
        .next_back()
        .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

/// `marker` starts a token in `line`, so it is not the end of a longer
/// name (`os.Exit(` and `process.exit(` are not `xit(`).
fn has_marker(line: &str, marker: &str) -> bool {
    line.match_indices(marker)
        .any(|(at, _)| token_start(line, at))
}

/// A test option that skips it, `{ skip: true }` or `{ skip: "why" }`
/// (Node, Bun), and not a field such as `{ skip: 10 }`.
fn skip_option(line: &str) -> bool {
    line.match_indices("skip:").any(|(at, marker)| {
        let value = line[at + marker.len()..].trim_start();
        token_start(line, at) && (value.starts_with("true") || value.starts_with(['"', '\'', '`']))
    })
}

/// An added line that skips or focuses a test.
fn skips_a_test(line: &str) -> bool {
    SKIP_MARKERS.iter().any(|m| has_marker(line, m)) || skip_option(line)
}

/// Markers that switch a check off for code.
const SUPPRESSIONS: &[&str] = &[
    "eslint-disable",
    "@ts-ignore",
    "@ts-expect-error",
    "@ts-nocheck",
    "# type: ignore",
    "# noqa",
    "#[allow(",
    "#![allow(",
    "// nolint",
    "//nolint",
    "# pylint: disable",
    "rubocop:disable",
    "@SuppressWarnings",
    "# pragma: no cover",
    "istanbul ignore",
    "c8 ignore",
];

fn test_definitions(text: &str) -> usize {
    text.lines()
        .map(str::trim_start)
        .filter(|line| {
            line.starts_with("#[test]")
                || line.starts_with("#[tokio::test")
                || line.starts_with("def test_")
                || line.starts_with("async def test_")
                || line.starts_with("it(")
                || line.starts_with("test(")
                // A skipped or focused test is still a test.
                || line.starts_with("it.")
                || line.starts_with("test.")
                || line.starts_with("xit(")
                || line.starts_with("xtest(")
                || line.starts_with("func Test")
                || line.starts_with("@Test")
        })
        .count()
}

fn assertions(text: &str) -> usize {
    text.lines()
        .map(str::trim_start)
        .filter(|line| {
            line.starts_with("assert")
                || line.starts_with("self.assert")
                || line.starts_with("expect(")
                || line.contains(" expect(")
                || line.starts_with("assert_eq!")
                || line.starts_with("assert!(")
                || line.contains(".should")
                || line.starts_with("require.")
                || line.starts_with("Assert.")
        })
        .count()
}

/// Lines added by the change, with their line numbers after it.
fn added_lines(before: &str, after: &str) -> Vec<(usize, String)> {
    let mut added = Vec::new();
    for hunk in crate::textdiff::hunks(before, after) {
        let value = hunk.to_json();
        let mut line = value["new_start"].as_u64().unwrap_or(1) as usize;
        for entry in value["lines"].as_array().into_iter().flatten() {
            match entry["kind"].as_str() {
                Some("add") => {
                    added.push((line, entry["text"].as_str().unwrap_or("").to_owned()));
                    line += 1;
                }
                Some("ctx") => line += 1,
                _ => {}
            }
        }
    }
    added
}

/// The flags of one changed file (`None`: missing before or after).
pub fn file_flags(path: &str, before: Option<&str>, after: Option<&str>) -> Vec<Flag> {
    let mut flags = Vec::new();
    let test = is_test_file(path);
    let flag = |kind: &'static str, line: Option<usize>, text: String| Flag {
        kind,
        path: path.to_owned(),
        line,
        text,
    };
    match (before, after) {
        (Some(old), None) => {
            if test && test_definitions(old) > 0 {
                flags.push(flag(
                    "deleted_tests",
                    None,
                    format!("deleted the test file {path}"),
                ));
            } else if is_ci_file(path) {
                flags.push(flag(
                    "ci_changed",
                    None,
                    format!("deleted the CI or hook file {path}"),
                ));
            }
            return flags;
        }
        (_, None) => return flags,
        _ => {}
    }
    let old = before.unwrap_or("");
    let new = after.unwrap_or("");
    if is_ci_file(path) {
        flags.push(flag(
            "ci_changed",
            None,
            if before.is_none() {
                format!("added the CI or hook file {path}")
            } else {
                format!("changed the CI or hook file {path}")
            },
        ));
    }
    if is_snapshot(path) && before.is_some() {
        flags.push(flag(
            "snapshots",
            None,
            format!("rewrote the test snapshot {path}"),
        ));
    }
    let added = added_lines(old, new);
    if test {
        let skipped: Vec<&(usize, String)> = added
            .iter()
            .filter(|(_, text)| skips_a_test(text))
            .collect();
        if let Some((line, _)) = skipped.first() {
            flags.push(flag(
                "skipped_tests",
                Some(*line),
                if skipped.len() == 1 {
                    format!("skipped or focused a test in {path}")
                } else {
                    format!("skipped or focused {} tests in {path}", skipped.len())
                },
            ));
        }
        let (tests_before, tests_after) = (test_definitions(old), test_definitions(new));
        if before.is_some() && tests_after < tests_before {
            let gone = tests_before - tests_after;
            flags.push(flag(
                "removed_tests",
                None,
                format!(
                    "removed {gone} test{} from {path}",
                    if gone == 1 { "" } else { "s" }
                ),
            ));
        }
        let (asserts_before, asserts_after) = (assertions(old), assertions(new));
        if before.is_some() && asserts_after < asserts_before && tests_after >= tests_before {
            let gone = asserts_before - asserts_after;
            flags.push(flag(
                "removed_assertions",
                None,
                format!(
                    "removed {gone} assertion{} from {path}",
                    if gone == 1 { "" } else { "s" }
                ),
            ));
        }
    }
    let suppressed: Vec<&(usize, String)> = added
        .iter()
        .filter(|(_, text)| SUPPRESSIONS.iter().any(|m| text.contains(m)))
        .collect();
    if let Some((line, _)) = suppressed.first() {
        flags.push(flag(
            "checks_disabled",
            Some(*line),
            if suppressed.len() == 1 {
                format!("switched off a lint or type check in {path}")
            } else {
                format!(
                    "switched off {} lint or type checks in {path}",
                    suppressed.len()
                )
            },
        ));
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    let loosened = (name.starts_with("tsconfig")
        && added
            .iter()
            .any(|(_, t)| t.contains("\"strict\"") && t.contains("false")))
        || (name.contains("eslint")
            && added
                .iter()
                .any(|(_, t)| t.contains("\"off\"") || t.contains(": 0")))
        || (is_ci_file(path)
            && added
                .iter()
                .any(|(_, t)| t.contains("continue-on-error: true") || t.contains("|| true")));
    if loosened {
        flags.push(flag(
            "checks_disabled",
            None,
            format!("loosened the checks configured in {path}"),
        ));
    }
    flags
}

/// The flags of a finished task, from its recorded changes.
pub fn for_task(store: &Store, workspace: &Workspace, task: &str) -> Vec<Flag> {
    let Ok(rows) = crate::review::files(store, workspace, task) else {
        return Vec::new();
    };
    let mut flags = Vec::new();
    for row in rows.iter().take(MAX_FILES) {
        let Some(path) = row["path"].as_str() else {
            continue;
        };
        if row["binary"] == true || row["status"] == "unchanged" || row["status"] == "unavailable" {
            continue;
        }
        let Ok((before, after)) = crate::review::contents(store, workspace, task, path) else {
            continue;
        };
        flags.extend(file_flags(path, before.as_deref(), after.as_deref()));
    }
    flags
}

/// `{count, text, flags}` for the job result and the `task.flags` event.
pub fn summary(flags: &[Flag]) -> Option<Value> {
    if flags.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    for flag in flags.iter().take(3) {
        parts.push(flag.text.clone());
    }
    let more = flags.len().saturating_sub(3);
    let mut text = format!("Heads up: this task {}", parts.join(", "));
    if more > 0 {
        text.push_str(&format!(" and {more} more"));
    }
    text.push('.');
    Some(json!({"count": flags.len(), "text": text, "flags": flags}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(flags: &[Flag]) -> Vec<&str> {
        flags.iter().map(|f| f.kind).collect()
    }

    #[test]
    fn skipped_and_focused_tests_are_flagged_with_their_line() {
        let before = "#[test]\nfn adds() {\n    assert_eq!(add(1, 2), 3);\n}\n";
        let after = "#[test]\n#[ignore]\nfn adds() {\n    assert_eq!(add(1, 2), 3);\n}\n";
        let flags = file_flags("tests/math.rs", Some(before), Some(after));
        assert_eq!(kinds(&flags), ["skipped_tests"]);
        assert_eq!(flags[0].line, Some(2));
        let js = file_flags(
            "src/app.test.ts",
            Some("it('works', () => {\n  expect(1).toBe(1);\n});\n"),
            Some("it.skip('works', () => {\n  expect(1).toBe(1);\n});\n"),
        );
        assert_eq!(kinds(&js), ["skipped_tests"]);
        let py = file_flags(
            "tests/test_api.py",
            Some("def test_a():\n    assert 1\n"),
            Some("@pytest.mark.skip\ndef test_a():\n    assert 1\n"),
        );
        assert_eq!(kinds(&py), ["skipped_tests"]);
        let node = file_flags(
            "test/api.test.js",
            Some("test('a', () => {});\n"),
            Some("test('a', { skip: true }, () => {});\n"),
        );
        assert_eq!(kinds(&node), ["skipped_tests"]);
    }

    #[test]
    fn exit_calls_and_pagination_fields_are_not_skips() {
        let go = file_flags(
            "foo_test.go",
            Some("package foo\n"),
            Some("package foo\n\nfunc TestMain(m *testing.M) { os.Exit(m.Run()) }\n"),
        );
        assert!(go.is_empty(), "{go:?}");
        let js = file_flags(
            "test/list.test.js",
            Some("it('lists', () => {\n  expect(list()).toEqual([]);\n});\n"),
            Some("it('lists', () => {\n  expect(list({ skip: 10 })).toEqual([]);\n  if (bad) process.exit(1);\n});\n"),
        );
        assert!(js.is_empty(), "{js:?}");
        let py = file_flags(
            "tests/test_main.py",
            Some("def test_a():\n    assert 1\n"),
            Some("def test_a():\n    assert 1\n\nsys.exit(pytest.main())\n"),
        );
        assert!(py.is_empty(), "{py:?}");
        // A real `xit(` still counts.
        let focused = file_flags(
            "src/app.test.ts",
            Some("it('works', () => {});\n"),
            Some("xit('works', () => {});\n"),
        );
        assert_eq!(kinds(&focused), ["skipped_tests"]);
    }

    #[test]
    fn deleted_and_removed_tests_and_weakened_assertions() {
        let test = "def test_a():\n    assert f() == 1\n    assert g() == 2\n\ndef test_b():\n    assert h()\n";
        assert_eq!(
            kinds(&file_flags("tests/test_x.py", Some(test), None)),
            ["deleted_tests"]
        );
        let fewer = "def test_a():\n    assert f() == 1\n    assert g() == 2\n";
        assert_eq!(
            kinds(&file_flags("tests/test_x.py", Some(test), Some(fewer))),
            ["removed_tests"]
        );
        let weaker = "def test_a():\n    assert f() == 1\n\ndef test_b():\n    assert h()\n";
        let flags = file_flags("tests/test_x.py", Some(test), Some(weaker));
        assert_eq!(kinds(&flags), ["removed_assertions"]);
        assert_eq!(flags[0].text, "removed 1 assertion from tests/test_x.py");
    }

    #[test]
    fn ci_suppressions_config_and_snapshots() {
        assert_eq!(
            kinds(&file_flags(
                ".github/workflows/ci.yml",
                Some("run: npm test\n"),
                Some("run: npm test || true\n")
            )),
            ["ci_changed", "checks_disabled"]
        );
        let code = file_flags(
            "src/app.ts",
            Some("const a: number = x;\n"),
            Some("// @ts-ignore\nconst a: number = x;\n"),
        );
        assert_eq!(kinds(&code), ["checks_disabled"]);
        assert_eq!(code[0].line, Some(1));
        let ts = file_flags(
            "tsconfig.json",
            Some("{\"compilerOptions\":{\n\"strict\": true\n}}\n"),
            Some("{\"compilerOptions\":{\n\"strict\": false\n}}\n"),
        );
        assert_eq!(kinds(&ts), ["checks_disabled"]);
        assert_eq!(
            kinds(&file_flags(
                "src/__snapshots__/a.test.ts.snap",
                Some("a"),
                Some("b")
            )),
            ["snapshots"]
        );
    }

    #[test]
    fn ordinary_changes_are_not_flagged() {
        // New tests, edited source, a suppression that was already there.
        assert!(file_flags(
            "tests/new_test.rs",
            None,
            Some("#[test]\nfn it_works() { assert!(true); }\n")
        )
        .is_empty());
        assert!(file_flags(
            "src/lib.rs",
            Some("fn a() {}\n"),
            Some("fn a() {}\nfn b() {}\n")
        )
        .is_empty());
        let kept = "#[allow(dead_code)]\nfn a() {}\n";
        assert!(file_flags(
            "src/lib.rs",
            Some(kept),
            Some(&format!("{kept}fn b() {{}}\n"))
        )
        .is_empty());
        // Skip markers outside tests (docs, source) are not about skipping tests.
        assert!(file_flags(
            "docs/guide.md",
            Some(""),
            Some("Use `it.skip(` to skip a test.\n")
        )
        .is_empty());
        assert_eq!(summary(&[]), None);
    }

    #[test]
    fn the_summary_names_the_first_findings() {
        let flags = vec![
            file_flags(
                "tests/a.rs",
                Some("#[test]\nfn a() {}\n"),
                Some("#[test]\n#[ignore]\nfn a() {}\n"),
            )
            .remove(0),
            file_flags(".github/workflows/ci.yml", Some("a\n"), Some("b\n")).remove(0),
        ];
        let summary = summary(&flags).unwrap();
        assert_eq!(
            summary["text"],
            "Heads up: this task skipped or focused a test in tests/a.rs, changed the CI or hook file .github/workflows/ci.yml."
        );
        assert_eq!(summary["count"], 2);
    }
}
