/** In-renderer changes whose API has no engine wake-up (editor saves and
 * revisions observed by the open editor). Workspace identities come from the
 * engine; never infer them from a relative file path or the current selection. */
const listeners = new Map<string, Set<() => void>>();

export function notifyWorkspaceFilesChanged(workspace: string) {
  for (const listener of [...(listeners.get(workspace) ?? [])]) listener();
}

export function onWorkspaceFilesChanged(
  workspace: string,
  listener: () => void,
) {
  let scoped = listeners.get(workspace);
  if (!scoped) listeners.set(workspace, (scoped = new Set()));
  scoped.add(listener);
  return () => {
    scoped.delete(listener);
    if (!scoped.size) listeners.delete(workspace);
  };
}
