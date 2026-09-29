// Editor recovery drafts (0.33.0 and later): an unsaved edit of README.md.
async fn editor_drafts(ctx: &mut Ctx) {
    let base = "# Fixture project\n";
    let body = json!({
        "workspace": ctx.project, "base": base, "draft": "# Fixture project\n\nUnsaved line.\n",
        "base_hash": shadowcode_core::workspace::hash(base.as_bytes()),
        "expected_revision": "missing",
    });
    match ctx.call("PUT", "/api/workspace/editor-draft?path=README.md", body).await {
        Ok(draft) => ctx.record("editor_draft", json!({"path": "README.md", "revision": draft["revision"]})),
        Err(error) => ctx.skip("editor draft", error),
    }
}
