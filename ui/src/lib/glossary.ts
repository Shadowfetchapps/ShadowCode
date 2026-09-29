/** Words ShadowCode uses, in plain language: the Help dialog lists them
 * and `<Term>` shows one on hover or focus. */
export const GLOSSARY = {
  agent:
    "The AI that reads your project, runs commands and edits files to do a task.",
  model:
    "The AI the agent thinks with. Some run on this computer (free and private), others in the cloud through a subscription or an API key.",
  "local model":
    "A model that runs on this computer: free, works offline, and your code never leaves the machine. It needs enough memory.",
  subscription:
    "A plan you already pay for, such as ChatGPT or Claude, used through its official app. ShadowCode uses your sign-in, not a key.",
  "API key":
    "A secret code from a provider, such as OpenRouter, that lets ShadowCode use its models. You pay for what you use.",
  token:
    "A small piece of text, about three quarters of a word. Models read and write in tokens, and cloud providers bill by them.",
  context:
    "Everything the model reads for a step: your message, the conversation so far and the files it opened. Each model can hold only so much.",
  compact:
    "Shorten a long conversation: earlier steps become a summary so it fits the model again. Type /compact.",
  approval:
    "A question before the agent does something that matters, such as running a command or deleting a file. You decide.",
  checkpoint:
    "A snapshot of your project taken before each step, so a task's changes can be undone.",
  rewind:
    "Put your files back the way they were before a task, from its checkpoints.",
  diff: "A file's before and after: removed lines in red, added lines in green.",
  hunk: "One block of changed lines in a diff. You can keep or undo each one.",
  commit:
    "A saved point in your project's Git history, with a message saying what changed.",
  branch:
    "A separate line of work in Git, so changes can be made without touching the main version.",
  "pull request":
    "A request to merge a branch's changes, reviewed on GitHub before it goes in.",
  worktree:
    "A second copy of your project in its own folder and branch, so a task can run beside other work without touching your files.",
  rules:
    "Instructions every agent gets for this project, such as its coding style or commands to avoid.",
  skill:
    "Saved instructions for one kind of task, which the agent uses when they fit.",
  "MCP server":
    "An add-on that gives the agent more tools, such as access to a database or an issue tracker.",
  sandbox:
    "A fence around the agent's commands that limits which files and which network they can reach.",
  role: "Which model does which step, such as a strong model to plan and a fast one to edit.",
} as const;

export type GlossaryWord = keyof typeof GLOSSARY;

export const GLOSSARY_WORDS = Object.keys(GLOSSARY).sort((a, b) =>
  a.localeCompare(b, undefined, { sensitivity: "base" }),
) as GlossaryWord[];
