# Models and routing in the native desktop

> **Advanced.** This is reached through `config.yaml` (`routing`); there is no desktop control. For the everyday workflow see the [user guide](USER_GUIDE.md), [subscriptions](SUBSCRIPTIONS.md) and [local models](LOCAL_MODELS.md). To give each step of a task its own model or vendor CLI in the desktop (plan, implement, review, explore), use [Roles](SUBAGENTS.md#roles).

Routing can choose a registered model for each task purpose when a task
starts without an explicit model (CLI `run` without `--model`, goal
milestones). Set `routing.enabled`, `planner`, `coder`, `reviewer` and `tester`
in `config.yaml` (`shadowcode config routing.enabled true`). The desktop
has no router control: the composer picker always sends an explicit target,
which overrides routing. The model note above the response names the model
that ran, for example "Using Cursor · Auto · Cloud".

The engine normally reserves a quarter of that window for a response (up to
8,192 tokens). If essential input leaves less room after history compaction,
it can request a shorter response down to 256 tokens. The actual provider
request uses that reduced limit; the configured context window never grows
silently. If input, tools and the minimum response still cannot fit, the task
reports the estimated requirement and stops before sending an oversized request.

Default goal milestones use Plan for inspection, Build for implementation, and
Test for required command verification. Planning and review retain read-only
permissions regardless of the selected model. A different model never grants
additional workspace or command permissions.

## Models with the same name

Discovered and newly configured models receive IDs derived from their provider,
endpoint, and model name. Two servers can offer the same model name without
overwriting each other. The model picker includes the endpoint when otherwise
identical choices need distinguishing. Existing saved IDs remain usable aliases;
custom aliases cannot be reassigned to a different model or server.

Discovery refreshes capabilities without overwriting a configured credential
environment variable or context limit. Switching the default keeps its previous
registered configuration available for routing. Credential values remain in
private secret storage; they are not included in task routing events.

Use registry IDs for API configuration. A bare model name is accepted only when
it resolves unambiguously, unless it is already an exact saved ID. Add a model
through **Custom model…** or configure it in Settings before assigning it to a
route. The native service also supports `POST /api/models/register` through IPC
for integrations that need to register a model without changing the default.

## Changes and failures

Routing selects a configuration when a task enters the queue. Changes affect
newly queued tasks; tasks already running or queued retain their selected model,
endpoint, credentials reference, and context limit. A future goal milestone uses
the routing configuration in effect when that milestone is queued.

An explicit route edit must name a registered coding model. If an older saved
configuration refers to a missing or ambiguous model, automatic selection uses
the default and records a **Using default** notice with the reason. The Health
drawer also shows the fallback. An unknown explicit composer/API selection is
rejected instead of silently changing the requested model.

A network, authentication, or provider response failure remains a failure for
the chosen provider. ShadowCode does not retry that task against another host.
Normal bounded retries, when configured, use the same chosen provider.

The legacy `architecture`, `small_edits`, `vision`, and `local` routing keys are
retained for configuration/API compatibility. They do not add new tool or image
input capabilities. The desktop exposes the four task modes above.

See [native verification](archive/NATIVE_VERIFICATION.md) for the tests and
[migration gates](archive/NATIVE_MIGRATION.md) for remaining release work.
