# OpenRouter

[OpenRouter](https://openrouter.ai) gives one API key access to hundreds of
hosted models. ShadowCode offers it for people without a subscription CLI, or
for trying a model before committing to one. Every token is billed to your
OpenRouter account. ShadowCode never presents these rows as a subscription.

## Add a key

1. Create a key at [openrouter.ai/keys](https://openrouter.ai/keys). Set a
   credit limit on the key there if you want a hard cap.
2. Open **Settings › Accounts**, find the **OpenRouter** card, paste the key
   and choose **Save**.

ShadowCode checks the key with OpenRouter (`GET /api/v1/key`) before storing
it. A rejected key is not saved. A saved key lives in the profile's
`secrets.env` (readable only by you) as `OPENROUTER_API_KEY`, and is never
shown again, logged, or passed to other tools. **Remove key** deletes it. An
`OPENROUTER_API_KEY` set in ShadowCode's own environment is used instead of
the stored key. Either way the key is removed from the environment of every
subscription CLI ShadowCode starts.

**Keep it in the keyring.** On a desktop with a keyring (GNOME Keyring,
KWallet, KeePassXC), **Settings › Accounts › Where your keys are kept** can
move the key out of `secrets.env` into the keyring, and back. The key is
written to the new place and read back before it is removed from the old
one. A locked keyring makes the key unavailable until you unlock it;
ShadowCode never unlocks it for you. Headless machines and SSH sessions keep
using `secrets.env`: saving or removing a key there that lives in the keyring
puts the new key in `secrets.env` and stops using the keyring's copy.

To test the keyring code against a real Secret Service without touching your
own keyring, run the ignored test in a private session:

```sh
t=$(mktemp -d); mkdir -p "$t/home" "$t/data" "$t/run"; chmod 700 "$t/run"
env -i PATH=/usr/bin:/bin HOME="$t/home" XDG_DATA_HOME="$t/data" XDG_RUNTIME_DIR="$t/run" \
  dbus-run-session -- sh -c 'echo -n test | gnome-keyring-daemon --unlock --components=secrets >/dev/null;
  exec target/debug/deps/shadowcode_core-* keyring::tests::live --ignored'
```

The card shows the key's label, credits used, its limit and what is left, the
account balance (`GET /api/v1/credits`), and how many models are available. A
key's limit is not the balance: a key can have most of its limit left while
the account has no credits, and then the card says *Out of credits*. **Open OpenRouter activity** links to
OpenRouter's own usage page.

## Pick a model

The picker's **API keys** group sits below **On this computer**, so a search
that matches both picks the local model first. Until a key is saved it offers
*Add an OpenRouter API key…* and fetches nothing. Saving a key fetches
OpenRouter's public model list and caches it in
`~/.local/state/shadow-agent/openrouter-models.json`. The picker shows the
cached list at once and refreshes it in the background when it is older than
six hours; **Refresh models** fetches it now. Each row has the ID
`api:openrouter:<slug>` and shows:

- the price per million input and output tokens, or *free*;
- *Vision* when the model accepts images;
- *Chat only* when the model doesn't support tool calls. Such a model can
  answer questions but can't read or edit files, because ShadowCode sends it
  no tools.

The group starts collapsed; use **Show all** or type in the search box (name
or slug, for example `qwen coder` or `qwen/qwen3-coder`).

## How tasks run

OpenRouter models run on ShadowCode's own agent loop, the one local models
use. Your permission mode and approvals, checkpoints, image attachments for
*Vision* models, the activity timeline and the review drawer all work the same
way. Turn on **Web** in the composer to give the task `web_fetch` and
`web_search`. The model's context window comes from OpenRouter's list, capped at
200,000 tokens.

Switching a conversation from a local model to OpenRouter sends earlier turns
to the cloud, so ShadowCode asks first, as it does for subscriptions.

## Cost and caching

ShadowCode asks OpenRouter for each request's cost and adds it up per job and
per conversation (`/cost` shows the totals). When a response carries no cost,
the model's listed prices are used and the total is marked as an estimate.
Claude and Gemini models get prompt-cache markers, so the unchanging start of
each request (instructions and tool list, and for Claude the conversation so
far) is billed at the cheaper cached rate on later steps. Cached tokens are
shown with the usage. Rate limits and overloaded providers are retried
automatically (see the [user guide](USER_GUIDE.md)).

## Offline and failures

- **Offline mode** turns the rows off and sends nothing to OpenRouter.
- **No key**: the group shows only *Add an OpenRouter API key…*, which opens
  **Settings › Accounts**. A task sent to an `api:openrouter:` ID without a
  key is refused before it starts.
- **Rejected or out of credit**: OpenRouter's error is shown on the task,
  for example "Model provider returned HTTP 402; the account is out of
  credits: This request requires more credits, or fewer max_tokens…".
