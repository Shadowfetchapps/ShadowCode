//! Shared fixed Python repair task and evaluator-owned five-case grader.
//! The task, initial source and tests must stay identical across live routes.
use anyhow::{ensure, Result};
use sha2::{Digest, Sha256};
use shadowcode_core::{
    config::ShellNetwork,
    process::{self, ProcessSpec},
    sandbox::{self, ShellPolicy},
};
use std::{fs, io::Read, path::Path, time::Duration};
use tokio_util::sync::CancellationToken;

pub const CHECK_COMMAND: &str = "python3 -m unittest -q";
pub const TASK: &str = "Fix helpers.py. clamp(x, lo, hi) must constrain x to the inclusive bounds and raise ValueError when lo > hi. unique(items) must preserve first-occurrence order, support unhashable elements, and leave the input unchanged. chunks(items, size) must return a list of consecutive chunks with a possibly shorter final chunk, return [] for empty input, leave the input unchanged, and raise ValueError for size <= 0. Inspect the files with tools. Edit only helpers.py; preserve test_helpers.py unchanged. Then run exactly `python3 -m unittest -q` with exec from the project root; explicitly set the project as cwd when the tool accepts a cwd/working-directory argument. Report the observed result.";
pub const SOURCE: &str = "def clamp(x, lo, hi):\n    return x\n\ndef unique(items):\n    return items\n\ndef chunks(items, size):\n    return [items]\n";
pub const TESTS: &str = r#"import unittest
from helpers import clamp, unique, chunks

class HelpersTest(unittest.TestCase):
    def test_clamp_bounds(self):
        self.assertEqual([clamp(x, -2, 4) for x in [-9, -2, 0, 4, 9]], [-2, -2, 0, 4, 4])
        self.assertEqual(clamp(3.5, 0.5, 2.5), 2.5)
        self.assertEqual(clamp(99, 2, 2), 2)
    def test_clamp_invalid(self):
        with self.assertRaises(ValueError): clamp(1, 4, 2)
    def test_unique(self):
        values = [3, 1, 3, 2, 1]
        self.assertEqual(unique(values), [3, 1, 2])
        self.assertEqual(values, [3, 1, 3, 2, 1])
        self.assertEqual(unique([]), [])
        self.assertEqual(unique([[1], [2], [1]]), [[1], [2]])
    def test_chunks(self):
        values = [1, 2, 3, 4, 5]
        self.assertEqual(chunks(values, 2), [[1, 2], [3, 4], [5]])
        self.assertEqual(chunks(values, 9), [values])
        self.assertEqual(chunks([], 3), [])
        self.assertEqual(values, [1, 2, 3, 4, 5])
    def test_chunks_invalid(self):
        for size in [0, -1]:
            with self.assertRaises(ValueError): chunks([1, 2], size)

if __name__ == '__main__': unittest.main()
"#;

pub fn provenance() -> serde_json::Value {
    let hash = |text: &str| format!("{:x}", Sha256::digest(text.as_bytes()));
    serde_json::json!({
        "task_sha256":hash(TASK), "source_sha256":hash(SOURCE),
        "tests_sha256":hash(TESTS), "grader_source_sha256":hash(include_str!("coding_fixture.rs")),
        "core_version":env!("CARGO_PKG_VERSION"), "test_command":CHECK_COMMAND,
        "test_cases":5
    })
}

pub async fn check(project: &Path) -> Result<process::ProcessResult> {
    ensure!(
        fs::symlink_metadata(project)?.is_dir(),
        "Expected a regular project directory"
    );
    let source = bounded_regular_text(&project.join("helpers.py"), 100_000)?;
    let verifier = tempfile::tempdir()?;
    // No model-created tests, import shadows, startup files or caches enter
    // the independent verifier. The model-requested check is separate.
    fs::write(verifier.path().join("helpers.py"), source)?;
    fs::write(verifier.path().join("test_helpers.py"), TESTS)?;
    fs::write(
        verifier.path().join("grade.py"),
        r#"import sys, pathlib, unittest
sys.path.insert(0, str(pathlib.Path(__file__).parent))
import test_helpers
suite = unittest.defaultTestLoader.loadTestsFromModule(test_helpers)
result = unittest.TextTestRunner(verbosity=1).run(suite)
passed = result.testsRun == 5 and result.wasSuccessful()
print('SHADOWCODE_EVALUATOR:5:PASS' if passed else 'SHADOWCODE_EVALUATOR:FAIL')
sys.exit(0 if passed else 1)
"#,
    )?;
    let prepared = sandbox::prepare_shell(
        verifier.path(),
        verifier.path(),
        "/usr/bin/python3 -I grade.py",
        &ShellPolicy {
            network: ShellNetwork::Off,
            allow: Vec::new(),
            allow_text: Vec::new(),
            home_binds: Vec::new(),
            require: true,
            landlock: true,
        },
    )?;
    let mut spec = ProcessSpec::command(&prepared.program, &[], verifier.path().to_owned());
    spec.timeout = Duration::from_secs(30);
    spec.output_limit = 256_000;
    spec.args = prepared.args;
    spec.child = prepared.child;
    let result = process::run(spec, CancellationToken::new(), None).await;
    if let Some(scratch) = prepared.scratch {
        sandbox::discard_scratch(&scratch)?;
    }
    result
}

// Consistency checks for this fixed fixture, not an adversarial-proof grader:
// generated Python still executes in the same process as the unittest runner.
pub fn assertions_passed(result: &process::ProcessResult) -> bool {
    result.ok
        && !result.timed_out
        && !result.cancelled
        && !result.truncated
        && result.stderr.contains("Ran 5 tests")
        && result
            .stdout
            .lines()
            .any(|line| line == "SHADOWCODE_EVALUATOR:5:PASS")
}

pub fn bounded_regular_text(path: &Path, limit: usize) -> Result<String> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= limit as u64,
        "Expected a bounded regular file: {}",
        path.display()
    );
    let mut text = String::new();
    file.take(limit as u64 + 1).read_to_string(&mut text)?;
    ensure!(text.len() <= limit, "File exceeded evaluator byte limit");
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CORRECT: &str = r#"def clamp(x, lo, hi):
    if lo > hi:
        raise ValueError("invalid bounds")
    return min(max(x, lo), hi)

def unique(items):
    result = []
    for item in items:
        if item not in result:
            result.append(item)
    return result

def chunks(items, size):
    if size <= 0:
        raise ValueError("invalid chunk size")
    return [items[start:start + size] for start in range(0, len(items), size)]
"#;

    #[tokio::test]
    async fn independent_verifier_accepts_correct_code_after_running_all_five_tests() {
        let project = tempfile::tempdir().unwrap();
        fs::write(project.path().join("helpers.py"), CORRECT).unwrap();
        let result = check(project.path()).await.unwrap();
        assert!(assertions_passed(&result), "{result:?}");
        assert!(result.stderr.contains("Ran 5 tests"), "{result:?}");
        assert!(!result.timed_out && !result.cancelled && !result.truncated);
    }

    #[tokio::test]
    async fn project_import_shadows_cannot_make_broken_helpers_pass() {
        let project = tempfile::tempdir().unwrap();
        fs::write(project.path().join("helpers.py"), SOURCE).unwrap();
        // These could turn `python3 -m unittest` in the submitted project into
        // a forged success. Neither file belongs in the independent grader.
        fs::write(
            project.path().join("unittest.py"),
            "print('SHADOWCODE_EVALUATOR:5:PASS')\n",
        )
        .unwrap();
        fs::write(
            project.path().join("sitecustomize.py"),
            "import os\nos._exit(0)\n",
        )
        .unwrap();
        fs::write(
            project.path().join("test_helpers.py"),
            "# assertions removed\n",
        )
        .unwrap();
        let result = check(project.path()).await.unwrap();
        assert!(!assertions_passed(&result), "{result:?}");
        assert!(!result.ok, "{result:?}");
        assert!(result.stderr.contains("Ran 5 tests"), "{result:?}");
        assert!(
            result.stdout.contains("SHADOWCODE_EVALUATOR:FAIL"),
            "{result:?}"
        );
        assert!(!result.stdout.contains("SHADOWCODE_EVALUATOR:5:PASS"));
    }

    #[tokio::test]
    async fn exit_zero_before_running_assertions_is_not_a_passing_grade() {
        let project = tempfile::tempdir().unwrap();
        for helper in [
            "import os\nos._exit(0)\n",
            "import os\nprint('SHADOWCODE_EVALUATOR:5:PASS', flush=True)\nos._exit(0)\n",
        ] {
            fs::write(project.path().join("helpers.py"), helper).unwrap();
            let result = check(project.path()).await.unwrap();
            assert!(result.ok && result.exit_code == 0, "{result:?}");
            assert!(!assertions_passed(&result), "{result:?}");
            assert!(!result.stderr.contains("Ran 5 tests"), "{result:?}");
        }
    }
}
