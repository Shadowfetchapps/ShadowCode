/** Plain words for the common ways a task fails, and what to do next. The
 * engine's own text stays below as the details. */

/** What a next-step button does. */
export type NextStep =
  /** Send the same request again. */
  | "retry"
  /** Continue this task on another model ("Try on…"). */
  | "try-on"
  /** Open the model picker. */
  | "choose-model"
  /** Settings › Local models (start or download a model). */
  | "open-local";

export type Diagnosis = {
  kind: string;
  title: string;
  plain: string;
  steps: NextStep[];
};

type Rule = Diagnosis & { match: RegExp; local?: boolean };

const LOCAL =
  /127\.0\.0\.1|localhost|\[::1\]|llama|ollama|local model|this computer/i;

// Order matters: the first matching rule wins.
const RULES: Rule[] = [
  {
    kind: "key",
    match:
      /HTTP (401|403)\b|check the API key|invalid[_ ]api[_ ]key|incorrect api key|no auth credentials|api key (is )?(missing|not set)|unauthori[sz]ed/i,
    title: "The provider didn't accept the key",
    plain:
      "The API key saved for this provider is missing, mistyped or was turned off. Add a working key in Settings › Accounts, or pick another model.",
    steps: ["choose-model", "try-on"],
  },
  {
    kind: "credits",
    match:
      /HTTP 402\b|needs credits|more credits|insufficient (credits|funds|balance|quota)|payment (method )?required/i,
    title: "The provider account is out of credits",
    plain:
      "The provider needs credits or a payment method before it answers. Add credits on the provider's website, or continue on a free model on this computer.",
    steps: ["try-on", "open-local"],
  },
  {
    kind: "context",
    match:
      /context (length|window|size)|maximum context|too many tokens|prompt is too long|exceeds the (model'?s )?(context|maximum)|n_ctx|input is too long/i,
    title: "The conversation is too long for this model",
    plain:
      "Everything the model has to read no longer fits in what it can hold at once. Type /compact and send your message again to shorten the conversation, start a new conversation, or continue on a model with a bigger context.",
    steps: ["try-on"],
  },
  {
    kind: "model",
    match:
      /HTTP 404\b|check the endpoint and model name|model[^.]{0,40}(not found|does not exist|is not available)|no such model|unknown model/i,
    title: "That model isn't available",
    plain:
      "The provider doesn't offer a model by that name (it may have been renamed or retired), or the address is wrong. Pick another model.",
    steps: ["choose-model", "try-on"],
  },
  {
    kind: "rate",
    match: /HTTP 429\b|rate limit|too many requests/i,
    title: "The provider is limiting requests",
    plain:
      "Too many requests reached the provider in a short time. Wait a minute and try again, or continue on another model.",
    steps: ["retry", "try-on"],
  },
  {
    kind: "overloaded",
    match:
      /HTTP (5\d\d)\b|overloaded|provider (is )?unavailable|service unavailable|bad gateway|internal server error/i,
    title: "The provider is having trouble",
    plain:
      "The provider's servers had a problem answering. It usually passes on its own: try again in a moment, or continue on another model.",
    steps: ["retry", "try-on"],
  },
  {
    kind: "local-down",
    local: true,
    match:
      /could not connect|connection refused|error sending request|failed to connect|connect(ion)? (failed|error)/i,
    title: "The model on this computer isn't running",
    plain:
      "ShadowCode couldn't reach the local model. Start it (or download one) in Settings › Local models, then try again.",
    steps: ["open-local", "retry"],
  },
  {
    kind: "offline",
    match:
      /could not connect|connection refused|error sending request|failed to connect|dns|network is unreachable|no route to host|timed out connecting/i,
    title: "ShadowCode couldn't reach the model",
    plain:
      "The connection to the model provider failed. Check that this computer is online (and any VPN or proxy), then try again, or use a model on this computer, which works offline.",
    steps: ["retry", "open-local"],
  },
  {
    kind: "stalled",
    match:
      /stalled|stopped sending|no (response|data) for|connection (closed|dropped|reset)|disconnected before/i,
    title: "The model stopped answering",
    plain:
      "The answer stopped arriving partway through. Nothing half-finished was applied. Try again, or continue on another model.",
    steps: ["retry", "try-on"],
  },
  {
    kind: "internal",
    match: /internal error/i,
    title: "ShadowCode hit a problem",
    plain:
      "Something went wrong inside ShadowCode, not with your project. Your files and this conversation were kept. Try again; if it keeps happening, Settings › About › Export a health report helps us fix it.",
    steps: ["retry"],
  },
];

/** A plain explanation of a failed task's text, or null when it isn't one
 * of the common failures. */
export function whatWentWrong(text: string): Diagnosis | null {
  const local = LOCAL.test(text);
  for (const rule of RULES) {
    if (rule.local && !local) continue;
    if (rule.match.test(text)) {
      const { kind, title, plain, steps } = rule;
      return { kind, title, plain, steps };
    }
  }
  return null;
}

export const STEP_LABELS: Record<NextStep, string> = {
  retry: "Try again",
  "try-on": "Continue on another model…",
  "choose-model": "Choose a model",
  "open-local": "Open Local models",
};
