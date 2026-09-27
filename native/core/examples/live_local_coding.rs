//! Opt-in coding evaluation with explicit installed GGUF files. No downloads,
//! user project changes, account access or network tools. Run under a loopback-
//! only network namespace to enforce offline inference. Reports failures too.
//! Usage: live_local_coding <report.json> <model.gguf> [model.gguf ...]
use anyhow::{ensure, Result};
use serde_json::{json, Value};
use shadowcode_core::{
    config::{Config, ShellNetwork},
    engine::Job,
    paths::AppPaths,
    process::{self, ProcessSpec},
    sandbox::{self, ShellPolicy},
    service::{Request, Service},
};
use std::{fs, path::Path, time::Duration};
use tokio_util::sync::CancellationToken;

const SOURCE: &str = "def clamp(x, lo, hi):\n    return x\n\ndef unique(items):\n    return items\n\ndef chunks(items, size):\n    return [items]\n";
const TESTS: &str = r#"import unittest
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

async fn check(project: &Path) -> Result<process::ProcessResult> {
    let metadata = fs::symlink_metadata(project.join("helpers.py"))?;
    ensure!(
        metadata.is_file() && metadata.len() < 100_000,
        "Expected a bounded regular helpers.py file"
    );
    let verifier = tempfile::tempdir()?;
    // No model-created tests, import shadows, startup files or caches enter
    // the independent verifier. The app's model-requested check is separate.
    fs::copy(
        project.join("helpers.py"),
        verifier.path().join("helpers.py"),
    )?;
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
fn assertions_passed(result: &process::ProcessResult) -> bool {
    result.ok
        && result.stderr.contains("Ran 5 tests")
        && result
            .stdout
            .lines()
            .any(|line| line == "SHADOWCODE_EVALUATOR:5:PASS")
}

async fn evaluate(file: &str, server: &str) -> Result<Value> {
    let root = tempfile::tempdir()?;
    let project = root.path().join("project");
    fs::create_dir(&project)?;
    fs::write(project.join("helpers.py"), SOURCE)?;
    fs::write(project.join("test_helpers.py"), TESTS)?;
    let baseline = check(&project).await?;
    ensure!(!baseline.ok && baseline.stdout.contains("SHADOWCODE_EVALUATOR:FAIL") && baseline.stderr.contains("Ran 5 tests"), "The evaluator must actually run and fail all five-case fixture tests before model execution: {}", baseline.stderr);
    let paths = AppPaths::isolated(&root.path().join("profile"))?;
    Config::patch(
        &paths,
        json!({
            "model":{"provider":"local","endpoint":"http://127.0.0.1:9/v1","name":"unused","context_limit":8192},
            "local_engine":{"llama_binary":server,"files":[file],"context_size":8192},
        "network":{"mode":"offline"},"cli_agents":{"enabled":false},
        "trusted_workspaces":[project],
        "sandbox":{"require":true,"home_binds":[]},
            "agent":{"max_steps":14,"max_task_tokens":50000,"tool_timeout_sec":30}
        }),
    )?;
    let service = Service::open(paths, Some(project.clone()))?;
    let catalog = service
        .dispatch(Request {
            method: "GET".into(),
            path: "/api/local-models".into(),
            body: Value::Null,
        })
        .await?;
    let model = catalog["models"]
        .as_array()
        .and_then(|m| m.iter().find(|m| m["path"] == file))
        .ok_or_else(|| anyhow::anyhow!("Explicit model missing from catalog"))?;
    let id = model["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Model ID missing"))?;
    let controller = service.engine.clone();
    let approved_project = project.clone();
    let approvals = tokio::spawn(async move {
        loop {
            for approval in controller.approvals().list(None) {
                let allow = approval.tool == "exec"
                    && approval.arguments["command"] == "python3 -m unittest -q"
                    && (matches!(approval.arguments["cwd"].as_str(), None | Some("" | "."))
                        || approval.arguments["cwd"].as_str() == approved_project.to_str());
                let _ = controller
                    .approvals()
                    .decide(&approval.id, &approval.session_id, allow);
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    });
    let outcome = async {
        let job: Job = serde_json::from_value(service.dispatch(Request {
            method:"POST".into(),path:"/api/jobs".into(),body:json!({
                "workspace":project,"model":id,"mode":"code","web":false,
                "task":"Fix helpers.py. clamp(x, lo, hi) must constrain x to the inclusive bounds and raise ValueError when lo > hi. unique(items) must preserve first-occurrence order, support unhashable elements, and leave the input unchanged. chunks(items, size) must return a list of consecutive chunks with a possibly shorter final chunk, return [] for empty input, leave the input unchanged, and raise ValueError for size <= 0. Inspect the files with tools. Edit only helpers.py; preserve test_helpers.py unchanged. Then run exactly `python3 -m unittest -q` with exec and no cwd parameter. Report the observed result."
            })
        }).await?)?;
        let done = match tokio::time::timeout(Duration::from_secs(240),service.engine.wait(&job.id)).await {
            Ok(done) => done?,
            Err(_) => {
                tokio::time::timeout(Duration::from_secs(15),service.engine.cancel(&job.id)).await??;
                tokio::time::timeout(Duration::from_secs(5),service.engine.wait(&job.id)).await??
            }
        };
        let preserved = fs::read_to_string(project.join("test_helpers.py"))? == TESTS;
        let independent = check(&project).await?;
        let assertions_passed = assertions_passed(&independent);
        let events = service.engine.store().recent_events(&job.session_id, 2000)?;
        let ran_check = events.iter().any(|e| e["type"]=="tool.completed"
            && e["payload"]["tool"]=="exec" && e["payload"]["success"]==true);
        Ok::<Value, anyhow::Error>(json!({
            "model":model,"hardware":catalog["hardware"],"runtime":catalog["runtime"],
            "job":done,"tests_preserved":preserved,"model_ran_successful_command":ran_check,
            "independent_check":independent,"baseline_check":baseline,
            "independent_assertions_passed":assertions_passed,
            "passed":done.status=="completed" && preserved && assertions_passed && ran_check,
            "source_after":fs::read_to_string(project.join("helpers.py"))?,"events":events
        }))
    }.await;
    approvals.abort();
    let cleanup = tokio::time::timeout(Duration::from_secs(15), service.engine.shutdown()).await;
    let cleanup_error = match cleanup {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(format!("{e:#}")),
        Err(_) => Some("shutdown exceeded 15 seconds".into()),
    };
    let mut report =
        outcome.unwrap_or_else(|e| json!({"file":file,"passed":false,"error":format!("{e:#}")}));
    if let Some(error) = cleanup_error {
        report["cleanup_error"] = json!(error);
        report["passed"] = json!(false);
    }
    Ok(report)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ensure!(
        args.len() >= 2,
        "usage: live_local_coding report.json model.gguf [model.gguf ...]"
    );
    let server = std::env::var("SHADOWCODE_LLAMA_SERVER")?;
    let mut reports = Vec::new();
    for file in &args[1..] {
        let result = match evaluate(file, &server).await {
            Ok(report) => report,
            Err(error) => json!({"file":file,"passed":false,"error":format!("{error:#}")}),
        };
        println!(
            "{} passed={} status={} error={}",
            file, result["passed"], result["job"]["status"], result["error"]
        );
        reports.push(result);
        fs::write(
            &args[0],
            serde_json::to_vec_pretty(&json!({
                "scope":"One small Python repair fixture per model; not a general coding benchmark",
                "context_tokens":8192,"max_steps":14,"timeout_seconds":240,"results":reports
            }))?,
        )?;
    }
    ensure!(
        reports.iter().all(|r| r["passed"] == true),
        "One or more models did not pass the coding fixture; inspect the report"
    );
    Ok(())
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
