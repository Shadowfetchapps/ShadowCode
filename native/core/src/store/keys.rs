//! Every `native_meta` key, and the `session_meta` keys more than one module
//! reads, built in one place. `native_meta` is a small key/value table that
//! also holds JSON documents (compare records, indexes, scoreboards); a key's
//! spelling is part of the on-disk format, so change none of these without a
//! migration.
use std::path::Path;

/// `native_meta`: the picker id last used in a project, for new
/// conversations there.
pub fn execution_target(workspace: &Path) -> String {
    format!("execution_target:{}", workspace.display())
}
/// `native_meta`: the last local GGUF model used in a project, preferred
/// when a subscription runs out and the task continues locally.
pub fn last_local_target(workspace: &Path) -> String {
    format!("last_local_target:{}", workspace.display())
}
/// `native_meta`: one compare record (JSON `compare::Record`).
pub fn compare_record(id: &str) -> String {
    format!("compare:{id}")
}
/// `native_meta`: a project's compare ids, newest first (JSON list).
pub fn compare_index(workspace: &Path) -> String {
    format!("compare_index:{}", workspace.display())
}
/// `native_meta`: a project's per-model compare wins and runs (JSON list).
pub fn compare_scoreboard(workspace: &Path) -> String {
    format!("compare_scoreboard:{}", workspace.display())
}
/// `native_meta`: files the user saved in the editor during a subscription
/// turn (JSON path → hash; `checkpoint::turn_edits`).
pub fn turn_edits(task: &str) -> String {
    format!("turn_edits:{task}")
}
/// `native_meta`: a rewind that can be undone (JSON `review::Rewind`); the
/// files as they were before it are checkpoint rows of task `rewind:<id>`.
pub fn rewind_undo(id: &str) -> String {
    format!("rewind_undo:{id}")
}
/// `native_meta`: one worktree task record (JSON `worktree_tasks::Record`).
pub fn worktree_task_record(id: &str) -> String {
    format!("worktree_task:{id}")
}
/// `native_meta`: a project's worktree task ids, newest first (JSON list).
pub fn worktree_task_index(workspace: &Path) -> String {
    format!("worktree_task_index:{}", workspace.display())
}
/// `native_meta`: the ShadowCode version that last opened this database
/// (since 1.0). An older version that refuses a newer database names it.
pub const APP_VERSION: &str = "app_version";
/// `native_meta`: one second opinion (JSON `second_opinion::Record`).
pub fn second_opinion(id: &str) -> String {
    format!("second_opinion:{id}")
}
/// `native_meta`: a project's second opinion ids, newest first (JSON list).
pub fn second_opinion_index(workspace: &Path) -> String {
    format!("second_opinion_index:{}", workspace.display())
}
/// `native_meta`: a project's second opinion preferences (JSON
/// `second_opinion::Prefs`: the reviewer last chosen, review before commit).
pub fn second_opinion_prefs(workspace: &Path) -> String {
    format!("second_opinion_prefs:{}", workspace.display())
}
/// `native_meta`: set once `goals.db` from before 0.28 was imported.
pub const LEGACY_GOALS_IMPORTED: &str = "legacy_goals_imported";
/// `native_meta`: set once the pre-0.28 background process list was imported.
pub const LEGACY_BACKGROUND_IMPORTED: &str = "legacy_background_imported";

/// `session_meta`: the picker id a conversation runs its next turn on.
pub const EXECUTION_TARGET: &str = "execution_target";
/// `session_meta`: the compare a lane conversation belongs to.
pub const COMPARE_ID: &str = "compare_id";
/// `session_meta`: the model id of a compare lane conversation.
pub const COMPARE_LANE: &str = "compare_lane";
/// `session_meta`: the worktree task a conversation belongs to. Its turns
/// run in that task's managed worktree until it is applied or discarded.
pub const WORKTREE_TASK: &str = "worktree_task";
/// `session_meta`: the project a worktree task's conversation belongs to.
pub const WORKTREE_SOURCE: &str = "worktree_source";
/// `session_meta`: the parent conversation of a subagent conversation.
pub const SUBAGENT_PARENT: &str = "subagent_parent";
/// `session_meta`: the subagent run a conversation belongs to.
pub const SUBAGENT_RUN: &str = "subagent_run";
/// `session_meta`: the agent definition a subagent conversation ran.
pub const SUBAGENT_AGENT: &str = "subagent_agent";
/// `session_meta`: the second opinion a hidden reviewer conversation ran.
pub const SECOND_OPINION: &str = "second_opinion";
/// `session_meta`: the conversation a second opinion belongs to; deleting
/// that conversation deletes the reviewer's conversation too.
pub const SECOND_OPINION_OF: &str = "second_opinion_of";
/// `session_meta`: the automation a conversation was started by.
pub const AUTOMATION_ID: &str = "automation_id";
/// `session_meta`: the automation run (history row) of a conversation.
pub const AUTOMATION_RUN: &str = "automation_run";
/// `session_meta`: set when the automation ran in a temporary managed
/// worktree, so opening its conversation never makes that folder a project.
pub const AUTOMATION_WORKTREE: &str = "automation_worktree";
/// `native_meta`: one subagent run (JSON `subagents::RunRecord`).
pub fn subagent_run(id: &str) -> String {
    format!("subagent:{id}")
}
/// `native_meta`: the subagent run ids of one parent conversation (JSON list).
pub fn subagent_index(parent_session: &str) -> String {
    format!("subagent_index:{parent_session}")
}
/// `native_meta`: an enabled MCP server's last tool list in a project.
pub fn mcp_catalog(workspace: &Path, server: &str) -> String {
    format!("mcp_catalog:{}:{server}", workspace.display())
}
/// `session_meta` prefix: a vendor CLI's own session/thread id, per vendor.
pub const NATIVE_SESSION_PREFIX: &str = "native_session:";
/// `session_meta`: a vendor CLI's own session/thread id for this conversation.
pub fn native_session(vendor: &str) -> String {
    format!("{NATIVE_SESSION_PREFIX}{vendor}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spellings are the on-disk format.
    #[test]
    fn keys_keep_their_stored_spelling() {
        let project = Path::new("/home/u/project");
        assert_eq!(
            execution_target(project),
            "execution_target:/home/u/project"
        );
        assert_eq!(
            last_local_target(project),
            "last_local_target:/home/u/project"
        );
        assert_eq!(compare_record("ab12"), "compare:ab12");
        assert_eq!(compare_index(project), "compare_index:/home/u/project");
        assert_eq!(
            compare_scoreboard(project),
            "compare_scoreboard:/home/u/project"
        );
        assert_eq!(worktree_task_record("ab12"), "worktree_task:ab12");
        assert_eq!(
            worktree_task_index(project),
            "worktree_task_index:/home/u/project"
        );
        assert_eq!(native_session("codex"), "native_session:codex");
        assert_eq!(APP_VERSION, "app_version");
        assert_eq!(rewind_undo("ab12"), "rewind_undo:ab12");
        assert_eq!(second_opinion("ab12"), "second_opinion:ab12");
        assert_eq!(
            second_opinion_index(project),
            "second_opinion_index:/home/u/project"
        );
        assert_eq!(
            second_opinion_prefs(project),
            "second_opinion_prefs:/home/u/project"
        );
        assert_eq!(SECOND_OPINION, "second_opinion");
        assert_eq!(SECOND_OPINION_OF, "second_opinion_of");
        assert_eq!(subagent_run("ab12"), "subagent:ab12");
        assert_eq!(subagent_index("s1"), "subagent_index:s1");
        assert_eq!(
            mcp_catalog(project, "config:x"),
            "mcp_catalog:/home/u/project:config:x"
        );
    }
}
