use super::*;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandRequest {
    pub command: String,
    pub timeout_sec: u64,
}
impl Engine {
    pub(crate) async fn start_command(
        &self,
        mut request: StartRequest,
        command: CommandRequest,
        owner: Option<&JobOwner>,
    ) -> Result<Job> {
        ensure!(
            !command.command.trim().is_empty()
                && command.command.len() <= 64_000
                && !command.command.contains('\0'),
            "Command must contain between 1 and 64000 bytes without NUL"
        );
        ensure!(
            (1..=3600).contains(&command.timeout_sec),
            "Command timeout must be between 1 and 3600 seconds"
        );
        request.mode = "command".into();
        request.task = format!("Run test command: {}", command.command);
        self.start_with_context(
            request,
            LaunchContext {
                command: Some(command),
                owner,
                permission_limit: Some(PermissionLevel::Workspace),
                ..Default::default()
            },
        )
        .await
    }
    pub(super) async fn run_command_job(
        &self,
        running: &Running,
        job: &Job,
        events: &TaskEvents,
        tools: &ToolExecutor,
        command: &CommandRequest,
    ) -> Result<(String, Value)> {
        // Store::create_job already recorded the prompt atomically.
        events.emit(
            "agent.started",
            json!({"task":job.task,"model":"native command","mode":"command"}),
        )?;
        let arguments = json!({"command":command.command,"timeout_sec":command.timeout_sec.min(running.config.agent.tool_timeout_sec)});
        let call = crate::models::ToolCall {
            id: crate::id(),
            name: "exec".into(),
            arguments: arguments.clone(),
        };
        let _checks_timing = running.clock.span(crate::timing::Section::FinalChecks);
        let mut result = crate::verification::execute(tools, call, &job.id, true).await?;
        running
            .clock
            .record_check(&result.output["verification_receipt"]);
        if result.success {
            let outcomes = tools
                .fire_hooks(hooks::context(
                    "on_complete",
                    "exec",
                    &arguments,
                    &result.output,
                    "",
                ))
                .await;
            let failure = match outcomes {
                Ok(outcomes) => hooks::failure(&outcomes),
                Err(error) => Some(error.to_string()),
            };
            if let Some(failure) = failure {
                result.success = false;
                result.error = format!("Completion checks failed: {failure}");
            }
        }
        // Durable transcript/export copy is redacted; `result` stays raw in memory.
        let mut completed = json!({"command":command.command,"success":result.success,"stdout":result.output["stdout"],"stderr":result.output["stderr"],"exit_code":result.output["exit_code"],"timed_out":result.output["timed_out"],"truncated":result.output["truncated"],"error":result.error});
        crate::redaction::redact_value(&mut completed);
        events.emit("command.completed", completed)?;
        let mut receipts = vec![result.output["verification_receipt"].clone()];
        if !result.success && receipts[0]["state"] == "passed" {
            receipts[0]["state"] = json!("failed");
            receipts[0]["success"] = json!(false);
        }
        crate::verification::refresh(&mut receipts, running.workspace.clone()).await;
        events.emit(
            "verification.summary",
            crate::verification::classify("", &receipts, false),
        )?;
        running.record().steps = 1;
        ensure!(result.success, "{}", result.error);
        Ok((
            format!(
                "Test command completed with exit status {}.",
                result.output["exit_code"]
            ),
            json!({"steps":[]}),
        ))
    }
}
