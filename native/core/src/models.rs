//! Streaming transports for local Ollama and compatible Chat Completions APIs.
use crate::{
    config::{secret, ModelConfig},
    paths::AppPaths,
};
use anyhow::{bail, ensure, Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const MAX_WIRE_BYTES: usize = 16_000_000;
const MAX_LINE_BYTES: usize = 1_000_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}
/// Token and cost accounting for one model request, or a sum of them (a job,
/// a session). `prompt_tokens` is the input and includes `cached_tokens`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    /// Input tokens the provider served from its prompt cache.
    pub cached_tokens: u64,
    /// Input tokens the provider wrote to its prompt cache, when reported.
    pub cache_write_tokens: u64,
    /// US dollars; `None` when no cost is known.
    pub cost_usd: Option<f64>,
    /// The cost is not entirely provider- or vendor-reported: it was worked
    /// out from the published price list, or some turns had no cost.
    pub cost_estimated: bool,
    /// Some token counts were estimated by ShadowCode, not reported.
    pub estimated: bool,
    /// Who reported the numbers: `provider` (API response), `local` (a model
    /// on this computer), `vendor` (a subscription CLI), `mixed`, or empty.
    pub source: String,
    /// Model requests (or vendor turns) counted in this total.
    pub turns: u64,
}
impl Usage {
    pub fn add(&mut self, other: &Self) {
        self.prompt_tokens = self.prompt_tokens.saturating_add(other.prompt_tokens);
        self.completion_tokens = self
            .completion_tokens
            .saturating_add(other.completion_tokens);
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
        self.cached_tokens = self.cached_tokens.saturating_add(other.cached_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(other.cache_write_tokens);
        let partial = (self.turns > 0 && self.cost_usd.is_none() && other.cost_usd.is_some())
            || (other.turns > 0 && other.cost_usd.is_none() && self.cost_usd.is_some());
        self.cost_usd = match (self.cost_usd, other.cost_usd) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        };
        self.cost_estimated |= other.cost_estimated || partial;
        self.estimated |= other.estimated;
        self.source = if self.turns == 0 || self.source.is_empty() {
            other.source.clone()
        } else if other.turns == 0 || other.source.is_empty() || other.source == self.source {
            std::mem::take(&mut self.source)
        } else {
            "mixed".into()
        };
        self.turns = self.turns.saturating_add(other.turns);
    }
    /// Remove an earlier total (for a task that is being re-finished).
    pub fn subtract(&mut self, other: &Self) {
        self.prompt_tokens = self.prompt_tokens.saturating_sub(other.prompt_tokens);
        self.completion_tokens = self
            .completion_tokens
            .saturating_sub(other.completion_tokens);
        self.total_tokens = self.total_tokens.saturating_sub(other.total_tokens);
        self.cached_tokens = self.cached_tokens.saturating_sub(other.cached_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_sub(other.cache_write_tokens);
        if let (Some(a), Some(b)) = (self.cost_usd, other.cost_usd) {
            self.cost_usd = Some((a - b).max(0.0));
        }
        self.turns = self.turns.saturating_sub(other.turns);
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ChatResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
    pub finish_reason: String,
    /// Tool calls whose arguments were repaired (`crate::tool_repair`).
    #[serde(default)]
    pub repaired: usize,
}

/// Content-free observations for one HTTP attempt. Unknown counters remain
/// absent; a failed or rejected response is never executable through this API.
#[derive(Clone, Debug, Serialize)]
pub struct RequestMetadata {
    pub schema_version: u8,
    pub configured_context_tokens: usize,
    pub requested_max_output_tokens: usize,
    pub request_limit_field: &'static str,
    pub request_bytes: usize,
    pub message_count: usize,
    pub tool_count: usize,
    pub tool_choice: Option<&'static str>,
    pub stream_requested: bool,
    pub usage_requested: bool,
    pub estimated_input_tokens: usize,
    pub estimate_method: &'static str,
    pub safety_margin_tokens: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct ObservedUsage {
    pub object_seen: bool,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub reported_total_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    /// Describes the latest non-null usage report, not an accounting outcome.
    pub status: &'static str,
    /// Exact complete report retained for failed-attempt accounting. Later
    /// partial or invalid reports cannot erase these already observed tokens.
    #[serde(
        rename = "retained_complete_report",
        serialize_with = "serialize_reported_usage"
    )]
    complete: Option<Usage>,
}

fn serialize_reported_usage<S>(usage: &Option<Usage>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    #[derive(Serialize)]
    struct Counts {
        prompt_tokens: u64,
        completion_tokens: u64,
        total_tokens: u64,
    }
    usage
        .as_ref()
        .map(|usage| Counts {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
        })
        .serialize(serializer)
}

impl Default for ObservedUsage {
    fn default() -> Self {
        Self {
            object_seen: false,
            prompt_tokens: None,
            completion_tokens: None,
            reported_total_tokens: None,
            cached_tokens: None,
            status: "unavailable",
            complete: None,
        }
    }
}

impl ObservedUsage {
    fn read(&mut self, value: &Value) {
        if value.is_null() {
            return;
        }
        self.object_seen |= value.is_object();
        if !value.is_object() {
            self.status = "invalid";
            return;
        }
        let prompt = value["prompt_tokens"].as_u64();
        let completion = value["completion_tokens"].as_u64();
        let total = value["total_tokens"].as_u64();
        // Retain each observed counter, but never synthesize a complete
        // accounting report by joining fields from separate frames.
        self.prompt_tokens = prompt.or(self.prompt_tokens);
        self.completion_tokens = completion.or(self.completion_tokens);
        self.reported_total_tokens = total.or(self.reported_total_tokens);
        self.cached_tokens = value["prompt_tokens_details"]["cached_tokens"]
            .as_u64()
            .or_else(|| value["prompt_cache_hit_tokens"].as_u64())
            .or(self.cached_tokens);
        if ["prompt_tokens", "completion_tokens", "total_tokens"]
            .iter()
            .any(|key| {
                value
                    .get(key)
                    .is_some_and(|v| !v.is_null() && v.as_u64().is_none())
            })
        {
            self.status = "invalid";
            return;
        }
        let (Some(prompt_tokens), Some(completion_tokens)) = (prompt, completion) else {
            self.status = "incomplete";
            return;
        };
        let Some(total_tokens) = prompt_tokens.checked_add(completion_tokens) else {
            self.status = "inconsistent";
            return;
        };
        if total.is_some_and(|reported| reported != total_tokens) {
            self.status = "inconsistent";
            return;
        }
        if self.complete.as_ref().is_some_and(|prior| {
            prompt_tokens < prior.prompt_tokens || completion_tokens < prior.completion_tokens
        }) {
            self.status = "inconsistent";
            return;
        }
        let mut usage = Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            ..Usage::default()
        };
        read_cache_and_cost(value, &mut usage);
        usage.cost_usd = usage.cost_usd.filter(|v| v.is_finite() && *v >= 0.0);
        self.status = "complete";
        self.complete = Some(usage);
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct RuntimeTimings {
    /// Runtime-processed tokens, excluding cached prompt tokens.
    pub prompt_n: Option<u64>,
    pub predicted_n: Option<u64>,
    pub prompt_ms: Option<f64>,
    pub predicted_ms: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResponseMetadata {
    pub schema_version: u8,
    pub accepted: bool,
    pub outcome: &'static str,
    pub failure_kind: Option<&'static str>,
    pub http_status: Option<u16>,
    pub wire_bytes: usize,
    pub content_bytes: usize,
    pub tool_call_slots: usize,
    pub tool_argument_bytes: usize,
    pub finish_reason: Option<&'static str>,
    pub finish_marker_seen: bool,
    pub stream_done_seen: bool,
    pub usage: ObservedUsage,
    pub runtime_timings: RuntimeTimings,
}
impl ResponseMetadata {
    /// Only a complete, internally consistent provider report can enter token
    /// accounting. This never estimates absent output or accepts partial tools.
    pub fn reported_usage(&self) -> Option<Usage> {
        self.usage.complete.clone()
    }
}

pub enum ModelObservation {
    Request(RequestMetadata),
    Response(Box<ResponseMetadata>),
}

#[derive(Clone)]
pub struct ModelClient {
    client: reqwest::Client,
    pub config: ModelConfig,
    key: Option<String>,
    extra_body: Option<Value>,
}
/// Rewrite JSON-schema `"type": ["string", "null"]` lists as `anyOf`. Some
/// GGUF chat templates (Gemma 4 under llama.cpp's Jinja engine) fail with
/// "filter-mapping not implemented" on type lists; `anyOf` is equivalent.
pub fn type_lists_to_any_of(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Array(types)) = map.get("type").cloned() {
                map.remove("type");
                map.insert(
                    "anyOf".into(),
                    Value::Array(types.into_iter().map(|t| json!({"type": t})).collect()),
                );
            }
            for child in map.values_mut() {
                type_lists_to_any_of(child);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(type_lists_to_any_of),
        _ => {}
    }
}

/// Loopback endpoints (local runtimes) must never go through an HTTP proxy.
/// What a provider said in an HTTP error reply, bounded and redacted. A
/// local runtime's reply is shown as it is; a remote provider's only as its
/// JSON error message (never an HTML error page).
pub fn provider_error_detail(body: &[u8], local: bool) -> Option<String> {
    let excerpt = String::from_utf8_lossy(&body[..body.len().min(16 * 1024)]).into_owned();
    let text = if local {
        String::from_utf8_lossy(&body[..body.len().min(600)]).into_owned()
    } else {
        let value: Value = serde_json::from_str(&excerpt).ok()?;
        let error = &value["error"];
        let message = error["message"]
            .as_str()
            .or_else(|| error.as_str())
            .or_else(|| value["message"].as_str())
            .or_else(|| value["detail"].as_str())?;
        crate::tools::truncate(message, 300).to_owned()
    };
    let text = crate::redaction::redact_text(text.trim()).text;
    (!text.is_empty()).then_some(text)
}
/// How long a provider may take to start answering (prompt processing on a
/// slow local model can take minutes). The answer itself has no overall
/// deadline, only the 120-second stall limit between bytes.
const HEADERS_TIMEOUT: Duration = Duration::from_secs(600);
/// Longest silence between bytes of a streamed answer.
const STREAM_STALL_TIMEOUT: Duration = Duration::from_secs(120);
/// Longest wait for a local runtime's first byte after its headers: prompt
/// processing on a CPU-only machine can take several minutes.
const LOCAL_FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(900);
pub fn is_loopback_endpoint(endpoint: &str) -> bool {
    reqwest::Url::parse(endpoint)
        .ok()
        .and_then(|url| {
            url.host_str().map(|host| {
                let host = host.trim_start_matches('[').trim_end_matches(']');
                host.eq_ignore_ascii_case("localhost")
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            })
        })
        .unwrap_or(false)
}
impl ModelClient {
    pub fn new(config: ModelConfig, paths: &AppPaths) -> Result<Self> {
        let validation = crate::config::Config {
            model: config.clone(),
            ..Default::default()
        };
        validation.validate()?;
        let key = secret(paths, &config.api_key_env)?;
        let endpoint = if config.endpoint.is_empty() {
            preset(&config.provider)["endpoint"]
                .as_str()
                .unwrap_or("")
                .to_owned()
        } else {
            config.endpoint.clone()
        };
        // No overall deadline: a long answer from a slow model may stream for
        // many minutes. The response must start within `HEADERS_TIMEOUT` and
        // the stream stalls after 120 seconds without bytes.
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none());
        if is_loopback_endpoint(&endpoint) {
            builder = builder.no_proxy();
        }
        Ok(Self {
            client: builder.build()?,
            config,
            key,
            extra_body: None,
        })
    }
    /// Per-launch bearer key held in memory (managed local runtime).
    pub fn with_bearer(mut self, key: Option<String>) -> Self {
        if key.is_some() {
            self.key = key;
        }
        self
    }
    /// Extra top-level request fields for compatible servers (for example
    /// `chat_template_kwargs` on llama.cpp).
    pub fn with_extra_body(mut self, extra: Option<Value>) -> Self {
        self.extra_body = extra.filter(Value::is_object);
        self
    }
    pub fn endpoint(&self) -> String {
        let endpoint = if self.config.endpoint.is_empty() {
            preset(&self.config.provider)["endpoint"]
                .as_str()
                .unwrap_or("")
                .to_owned()
        } else {
            self.config.endpoint.clone()
        };
        endpoint.trim_end_matches('/').to_owned()
    }
    pub fn request_body(&self, messages: &[Value], tools: &[Value], max_tokens: usize) -> Value {
        let messages: Vec<Value> = messages
            .iter()
            .map(|message| {
                let mut message = message.clone();
                if let Some(object) = message.as_object_mut() {
                    object.retain(|key, _| !key.starts_with("_shadow_"));
                }
                message
            })
            .collect();
        if self.config.provider == "ollama" {
            // Common installed Ollama templates (including Qwen3) render only
            // the leading system block and skip later system-role messages.
            // Preserve runtime repair/compaction notes there, with their original
            // position labelled so historical failures do not look newly issued.
            let guidance: Vec<_> = messages.iter().enumerate().filter(|(_, m)| m["role"] == "system").map(|(index, m)| {
                let text = m["content"].as_str().unwrap_or("");
                if index == 0 { text.to_owned() } else {
                    format!("[Runtime note at conversation position {index}; later messages may resolve it.]\n{text}")
                }
            }).collect();
            let mut converted = Vec::new();
            if !guidance.is_empty() {
                converted.push(json!({"role":"system", "content":guidance.join("\n\n")}));
            }
            converted.extend(messages.iter().filter(|m| m["role"] != "system").map(|m| {
                let mut m = m.clone();
                if m["role"] == "tool" {
                    if let Some(name) = m.get("name").cloned() {
                        m["tool_name"] = name;
                    }
                    if let Some(obj) = m.as_object_mut() {
                        obj.remove("tool_call_id");
                    }
                }
                if let Some(calls) = m.get_mut("tool_calls").and_then(Value::as_array_mut) {
                    for call in calls {
                        if let Some(args) =
                            call.pointer("/function/arguments").and_then(Value::as_str)
                        {
                            if let Ok(args) = serde_json::from_str::<Value>(args) {
                                call["function"]["arguments"] = args;
                            }
                        }
                    }
                }
                m
            }));
            let keep_alive = match self.config.keep_alive.as_str() {
                "-1" => json!(-1),
                "0" => json!(0),
                duration => json!(duration),
            };
            let mut body = json!({
                "model": self.config.name,
                "messages": converted,
                "stream": true,
                "think": false,
                "keep_alive": keep_alive,
                "options": {
                    "num_predict": max_tokens,
                    "num_ctx": self.config.context_limit
                },
            });
            if !tools.is_empty() {
                body["tools"] = json!(tools);
            }
            body
        } else {
            let mut messages = messages;
            crate::prompt_cache::apply(&self.config, &mut messages);
            let mut body = json!({"model":self.config.name,"messages":messages,"stream":true,"stream_options":{"include_usage":true},"max_tokens":max_tokens});
            if self.config.provider == crate::openrouter::PROVIDER {
                // Ask OpenRouter for the request's cost in `usage.cost`.
                body["usage"] = json!({"include": true});
            }
            if !tools.is_empty() {
                body["tools"] = if self.config.provider == "llamacpp" {
                    json!(tools
                        .iter()
                        .map(|t| {
                            let mut t = t.clone();
                            type_lists_to_any_of(&mut t);
                            t
                        })
                        .collect::<Vec<_>>())
                } else {
                    json!(tools)
                };
                body["tool_choice"] = json!("auto");
            }
            if let (Some(Value::Object(extra)), Some(object)) =
                (&self.extra_body, body.as_object_mut())
            {
                for (key, value) in extra {
                    object.entry(key.clone()).or_insert_with(|| value.clone());
                }
            }
            body
        }
    }
    /// Cancellation drops the HTTP body immediately. Once any response bytes
    /// arrive, the caller must not retry invisibly: partial tool arguments must
    /// never execute, and duplicate assistant text must not be appended.
    pub async fn chat<F>(
        &self,
        messages: &[Value],
        tools: &[Value],
        cancel: CancellationToken,
        text: F,
    ) -> Result<ChatResponse>
    where
        F: FnMut(&str) + Send,
    {
        self.chat_bounded(messages, tools, cancel, None, text).await
    }
    /// [`Self::chat`] with the response capped at `max_tokens` (for short
    /// internal requests such as a compaction summary).
    pub async fn chat_bounded<F>(
        &self,
        messages: &[Value],
        tools: &[Value],
        cancel: CancellationToken,
        max_tokens: Option<usize>,
        text: F,
    ) -> Result<ChatResponse>
    where
        F: FnMut(&str) + Send,
    {
        self.chat_bounded_observed(messages, tools, cancel, max_tokens, text, |_| {})
            .await
    }
    /// Like chat, with bounded content-free receipts even for rejected output.
    pub async fn chat_observed<F, O>(
        &self,
        messages: &[Value],
        tools: &[Value],
        cancel: CancellationToken,
        text: F,
        observe: O,
    ) -> Result<ChatResponse>
    where
        F: FnMut(&str) + Send,
        O: FnMut(ModelObservation) + Send,
    {
        self.chat_bounded_observed(messages, tools, cancel, None, text, observe)
            .await
    }
    /// Existing wrappers retain their result/error semantics. Observations do
    /// not contain prompts, output, arguments, credentials, or provider URLs.
    pub async fn chat_bounded_observed<F, O>(
        &self,
        messages: &[Value],
        tools: &[Value],
        cancel: CancellationToken,
        max_tokens: Option<usize>,
        mut text: F,
        mut observe: O,
    ) -> Result<ChatResponse>
    where
        F: FnMut(&str) + Send,
        O: FnMut(ModelObservation) + Send,
    {
        ensure!(
            self.config.provider != "mock",
            "Offline demonstrations are handled by the agent fixture"
        );
        let ollama = self.config.provider == "ollama";
        let endpoint = self.endpoint();
        let url = if ollama {
            format!("{}/api/chat", endpoint.trim_end_matches("/v1"))
        } else {
            format!(
                "{}/chat/completions",
                if endpoint.ends_with("/v1") {
                    endpoint
                } else {
                    format!("{endpoint}/v1")
                }
            )
        };
        let response_tokens =
            crate::context::response_budget(messages, tools, self.config.context_limit)?
                .min(max_tokens.unwrap_or(usize::MAX).max(1));
        let body = self.request_body(messages, tools, response_tokens);
        observe(ModelObservation::Request(RequestMetadata {
            schema_version: 1,
            configured_context_tokens: self.config.context_limit,
            requested_max_output_tokens: response_tokens,
            request_limit_field: if ollama { "num_predict" } else { "max_tokens" },
            request_bytes: serde_json::to_vec(&body)?.len(),
            message_count: body["messages"].as_array().map_or(0, Vec::len),
            tool_count: body["tools"].as_array().map_or(0, Vec::len),
            tool_choice: body.get("tool_choice").map(|choice| match choice.as_str() {
                Some("auto") => "auto",
                Some("none") => "none",
                Some("required") => "required",
                _ => "other",
            }),
            stream_requested: body["stream"] == true,
            usage_requested: !ollama && body["stream_options"]["include_usage"] == true,
            estimated_input_tokens: crate::context::estimate_tokens(&json!(messages))
                + crate::context::estimate_tokens(&json!(tools)),
            estimate_method: "deterministic_char_div3_before_runtime_template",
            safety_margin_tokens: 256,
        }));
        let mut request = self.client.post(&url).json(&body);
        if self.config.provider == crate::openrouter::PROVIDER {
            // Optional app attribution OpenRouter documents for its rankings.
            request = request
                .header(
                    "HTTP-Referer",
                    "https://github.com/Shadowfetchapps/ShadowCode",
                )
                .header("X-Title", "ShadowCode");
        }
        if let Some(key) = &self.key {
            request = request.bearer_auth(key);
        }
        let mut decoder = StreamDecoder::new(ollama);
        let mut bytes = 0;
        let mut http_status = None;
        let transport: Result<()> = async {
            ensure!(!cancel.is_cancelled(), "Model request cancelled");
            let response = tokio::select! {
                _ = cancel.cancelled() => bail!("Model request cancelled"),
                response = tokio::time::timeout(HEADERS_TIMEOUT, request.send()) => response
                    .map_err(|_| crate::retry::ModelFailure::Stalled {
                        message: format!(
                            "The model provider did not start its response within {} minutes",
                            HEADERS_TIMEOUT.as_secs() / 60
                        ),
                    })?
                    .context("Could not connect to the model provider")?,
            };
            let status = response.status();
            http_status = Some(status.as_u16());
            if !status.is_success() {
                let code = status.as_u16();
                let retry_after = crate::retry::retry_after(response.headers());
                // Providers explain rejections: a local runtime in text (for
                // example a prompt larger than the context window), OpenRouter
                // and OpenAI-style APIs as JSON `error.message` (for example
                // "This request requires more credits"). Read at most 16 KiB
                // and show a bounded, redacted excerpt.
                let local = is_loopback_endpoint(&url);
                let diagnostic_bytes = tokio::time::timeout(Duration::from_secs(5), async {
                    let mut body = Vec::new();
                    let mut stream = response.bytes_stream();
                    while let Some(Ok(chunk)) = stream.next().await {
                        body.extend_from_slice(&chunk);
                        if body.len() >= 16 * 1024 {
                            break;
                        }
                    }
                    body
                })
                .await
                .unwrap_or_default();
                bytes = diagnostic_bytes.len();
                let detail = provider_error_detail(&diagnostic_bytes, local)
                    .map(|text| format!(": {text}"))
                    .unwrap_or_default();
                return Err(anyhow::Error::new(crate::retry::ModelFailure::Http {
                    status: code,
                    retry_after,
                    local: is_loopback_endpoint(&url),
                    message: format!(
                        "Model provider returned HTTP {code}{}{detail}",
                        match code {
                            401 | 403 => "; check the API key",
                            402 => "; the provider account needs credits or a payment method",
                            404 => "; check the endpoint and model name",
                            429 => "; provider rate limit reached",
                            503 | 529 => "; provider overloaded",
                            _ => "",
                        }
                    ),
                }));
            }
            let json_response = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
                .is_some_and(|h| h.starts_with("application/json"));
            let mut stream = response.bytes_stream();
            let mut raw = Vec::new();
            // A local runtime sends its headers at once and then says nothing
            // while it reads the prompt, which on a CPU can take minutes for a
            // long context. Allow that before the first byte; afterwards the
            // usual stall limit applies between bytes.
            let mut waiting_for_first_byte = is_loopback_endpoint(&url);
            loop {
                let limit = if waiting_for_first_byte {
                    LOCAL_FIRST_BYTE_TIMEOUT
                } else {
                    STREAM_STALL_TIMEOUT
                };
                let next = tokio::select! {_=cancel.cancelled()=>bail!("Model request cancelled"),next=tokio::time::timeout(limit,stream.next())=>next.map_err(|_| crate::retry::ModelFailure::Stalled{message:format!("Model response stalled for {} seconds", limit.as_secs())})?};
                waiting_for_first_byte = false;
                let Some(chunk) = next else { break };
                let chunk = chunk.context("Model stream disconnected before completion")?;
                bytes += chunk.len();
                ensure!(
                    bytes <= MAX_WIRE_BYTES,
                    "Model response exceeded the 16 MB limit"
                );
                if json_response {
                    raw.extend_from_slice(&chunk);
                } else {
                    for delta in decoder.push(&chunk)? {
                        text(&delta);
                    }
                    if decoder.done {
                        break;
                    }
                }
            }
            if json_response {
                let value: Value =
                    serde_json::from_slice(&raw).context("Provider returned invalid JSON")?;
                decoder.full_response(value)?;
                text(&decoder.response.text);
            } else {
                for delta in decoder.flush()? {
                    text(&delta);
                }
            }
            Ok(())
        }.await;
        // Snapshot before finish consumes the decoder and rejects cut-short
        // responses. Also retain observations from transport/parser failures.
        let mut metadata = decoder.metadata(http_status, bytes);
        let transport_failed = transport.is_err();
        let result = transport.and_then(|()| decoder.finish());
        let completion_rejected = !transport_failed
            && matches!(
                metadata.finish_reason,
                Some("length" | "max_tokens" | "content_filter")
            );
        metadata.accepted = result.is_ok();
        metadata.failure_kind = result.as_ref().err().map(|error| {
            if let Some(reason) = crate::retry::classify(error) {
                reason.kind
            } else if cancel.is_cancelled() {
                "cancelled"
            } else if completion_rejected {
                "completion_rejected"
            } else {
                "invalid_response"
            }
        });
        metadata.outcome = if result.is_ok() {
            "accepted"
        } else if cancel.is_cancelled() {
            "cancelled"
        } else if http_status.is_some_and(|status| !(200..300).contains(&status)) {
            "http_rejected"
        } else if completion_rejected {
            "completion_rejected"
        } else {
            "transport_or_protocol_error"
        };
        observe(ModelObservation::Response(Box::new(metadata)));
        result
    }
    pub async fn test(&self, cancel: CancellationToken) -> Result<Value> {
        let start = std::time::Instant::now();
        let response = self
            .chat(
                &[json!({"role":"user","content":"Reply with exactly: ShadowCode connected"})],
                &[],
                cancel,
                |_| {},
            )
            .await?;
        ensure!(
            !response.text.trim().is_empty(),
            "Model returned an empty reply"
        );
        Ok(
            json!({"ok":true,"latency_ms":start.elapsed().as_millis(),"reply":response.text,"model":self.config.name,"usage":response.usage}),
        )
    }
}

#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    args: String,
}

/// Incremental wire parser. Bytes may split anywhere, including inside UTF-8,
/// JSON escapes, SSE frames, or a function's arguments.
pub struct StreamDecoder {
    ollama: bool,
    pending: Vec<u8>,
    sse_data: Vec<String>,
    response: ChatResponse,
    calls: BTreeMap<usize, PartialCall>,
    pub done: bool,
    seen_finish: bool,
    duplicate_call_ids: bool,
    observed_usage: ObservedUsage,
    runtime_timings: RuntimeTimings,
}
impl StreamDecoder {
    pub fn new(ollama: bool) -> Self {
        Self {
            ollama,
            pending: Vec::new(),
            sse_data: Vec::new(),
            response: ChatResponse::default(),
            calls: BTreeMap::new(),
            done: false,
            seen_finish: false,
            duplicate_call_ids: false,
            observed_usage: ObservedUsage::default(),
            runtime_timings: RuntimeTimings::default(),
        }
    }
    pub fn metadata(&self, http_status: Option<u16>, wire_bytes: usize) -> ResponseMetadata {
        let finish_reason = if !self.seen_finish {
            None
        } else {
            Some(match self.response.finish_reason.as_str() {
                "stop" => "stop",
                "tool_calls" => "tool_calls",
                "function_call" => "function_call",
                "length" => "length",
                "max_tokens" => "max_tokens",
                "content_filter" => "content_filter",
                _ => "other",
            })
        };
        ResponseMetadata {
            schema_version: 1,
            accepted: false,
            outcome: "pending",
            failure_kind: None,
            http_status,
            wire_bytes,
            content_bytes: self.response.text.len(),
            tool_call_slots: self.calls.len(),
            tool_argument_bytes: self.calls.values().map(|call| call.args.len()).sum(),
            finish_reason,
            finish_marker_seen: self.seen_finish,
            stream_done_seen: self.done,
            usage: self.observed_usage.clone(),
            runtime_timings: self.runtime_timings.clone(),
        }
    }
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>> {
        ensure!(
            bytes.len() <= MAX_WIRE_BYTES,
            "Model stream chunk exceeded 16 MB"
        );
        let mut pending = std::mem::take(&mut self.pending);
        pending.extend_from_slice(bytes);
        let mut text = Vec::new();
        let mut start = 0;
        while let Some(length) = pending[start..].iter().position(|b| *b == b'\n') {
            ensure!(length <= MAX_LINE_BYTES, "Model stream frame exceeded 1 MB");
            let end = start + length;
            let line = std::str::from_utf8(&pending[start..=end])
                .context("Model stream is not UTF-8")?
                .trim_end_matches(['\n', '\r']);
            self.line(line, &mut text)?;
            start = end + 1;
        }
        self.pending.extend_from_slice(&pending[start..]);
        ensure!(
            self.pending.len() <= MAX_LINE_BYTES,
            "Model stream frame exceeded 1 MB"
        );
        Ok(text)
    }
    fn line(&mut self, line: &str, out: &mut Vec<String>) -> Result<()> {
        if self.done {
            return Ok(());
        }
        if self.ollama {
            if !line.trim().is_empty() {
                self.chunk(
                    serde_json::from_str(line).context("Invalid Ollama stream frame")?,
                    out,
                )?;
            }
        } else if line.is_empty() {
            self.frame(out)?;
        } else if let Some(data) = line.strip_prefix("data:") {
            self.sse_data
                .push(data.strip_prefix(' ').unwrap_or(data).into());
            ensure!(
                self.sse_data.len() <= 10_000
                    && self.sse_data.iter().map(String::len).sum::<usize>() <= MAX_LINE_BYTES,
                "SSE event exceeded 1 MB"
            );
        }
        Ok(())
    }
    fn frame(&mut self, out: &mut Vec<String>) -> Result<()> {
        if self.sse_data.is_empty() {
            return Ok(());
        }
        let data = std::mem::take(&mut self.sse_data).join("\n");
        if data.trim() == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        self.chunk(
            serde_json::from_str(&data).context("Invalid compatible stream frame")?,
            out,
        )
    }
    pub fn flush(&mut self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        if !self.pending.is_empty() {
            let bytes = std::mem::take(&mut self.pending);
            let line = std::str::from_utf8(&bytes).context("Truncated UTF-8 stream")?;
            self.line(line.trim_end_matches('\r'), &mut out)?;
        }
        if !self.ollama {
            self.frame(&mut out)?;
        }
        Ok(out)
    }
    fn observe_fields(&mut self, value: &Value) {
        if self.ollama {
            if value.get("prompt_eval_count").is_some() || value.get("eval_count").is_some() {
                self.observed_usage.read(&json!({
                    "prompt_tokens": value["prompt_eval_count"],
                    "completion_tokens": value["eval_count"],
                }));
            }
        } else if let Some(usage) = value.get("usage") {
            self.observed_usage.read(usage);
        }
        if let Some(timings) = value.get("timings").filter(|v| v.is_object()) {
            self.runtime_timings.prompt_n = timings["prompt_n"]
                .as_u64()
                .or(self.runtime_timings.prompt_n);
            self.runtime_timings.predicted_n = timings["predicted_n"]
                .as_u64()
                .or(self.runtime_timings.predicted_n);
            for (key, slot) in [
                ("prompt_ms", &mut self.runtime_timings.prompt_ms),
                ("predicted_ms", &mut self.runtime_timings.predicted_ms),
            ] {
                if let Some(value) = timings[key].as_f64().filter(|v| v.is_finite() && *v >= 0.0) {
                    *slot = Some(value);
                }
            }
        }
    }
    fn chunk(&mut self, value: Value, out: &mut Vec<String>) -> Result<()> {
        self.observe_fields(&value);
        if let Some(error) = value.get("error").filter(|e| !e.is_null()) {
            let code = error["code"]
                .as_u64()
                .or_else(|| error["code"].as_str().and_then(|c| c.parse().ok()))
                .and_then(|c| u16::try_from(c).ok());
            let detail = error["message"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| error.to_string());
            let detail = crate::tools::truncate(&detail, 300).to_owned();
            let shown = crate::redaction::redact_text(detail.trim()).text;
            return Err(anyhow::Error::new(crate::retry::ModelFailure::Stream {
                code,
                message: if shown.is_empty() {
                    "Provider reported an error while generating".into()
                } else {
                    format!("Provider reported an error while generating: {shown}")
                },
                detail,
            }));
        }
        let message = if self.ollama {
            &value["message"]
        } else {
            &value["choices"][0]["delta"]
        };
        if let Some(text) = message["content"].as_str() {
            self.response.text.push_str(text);
            if !text.is_empty() {
                out.push(text.into());
            }
        }
        ensure!(
            self.response.text.len() <= 4_000_000,
            "Model output exceeded the text limit"
        );
        if let Some(calls) = message["tool_calls"].as_array() {
            // Separate entries in one array must not coalesce through the
            // unindexed continuation heuristic. Record the malformed group
            // until finish so later reported usage still reaches accounting.
            // Repeated identity in a later frame remains valid continuation.
            let mut frame_ids = BTreeSet::new();
            self.duplicate_call_ids |= calls
                .iter()
                .filter_map(|call| call["id"].as_str().filter(|id| !id.is_empty()))
                .any(|id| !frame_ids.insert(id));
            for (position, call) in calls.iter().enumerate() {
                let index = self.resolve_call_index(call, position, calls.len());
                ensure!(index < 128, "Too many tool calls in one response");
                let part = self.calls.entry(index).or_default();
                if let Some(id) = call["id"].as_str() {
                    if part.id.is_empty() {
                        part.id.push_str(id);
                    } else {
                        ensure!(part.id == id, "Tool call id changed during streaming");
                    }
                }
                if let Some(name) = call["function"]["name"].as_str() {
                    if part.name.is_empty() {
                        part.name.push_str(name);
                    } else if part.name != name && !name.is_empty() {
                        if name.starts_with(&part.name) {
                            part.name = name.to_owned();
                        } else if !part.name.ends_with(name) {
                            part.name.push_str(name);
                        }
                    }
                }
                if let Some(args) = call["function"].get("arguments") {
                    if let Some(args) = args.as_str() {
                        part.args.push_str(args);
                    } else {
                        part.args = args.to_string();
                    }
                }
                ensure!(
                    part.args.len() <= MAX_LINE_BYTES
                        && part.name.len() <= 256
                        && part.id.len() <= 512,
                    "Tool call exceeded limits"
                );
            }
        }
        if self.ollama {
            if value["done"].as_bool() == Some(true) {
                self.done = true;
                self.seen_finish = true;
                self.response.finish_reason =
                    value["done_reason"].as_str().unwrap_or("stop").into();
            }
            self.response.usage.prompt_tokens = value["prompt_eval_count"]
                .as_u64()
                .unwrap_or(self.response.usage.prompt_tokens);
            self.response.usage.completion_tokens = value["eval_count"]
                .as_u64()
                .unwrap_or(self.response.usage.completion_tokens);
        } else {
            if let Some(reason) = value["choices"][0]["finish_reason"].as_str() {
                self.response.finish_reason = reason.into();
                self.seen_finish = true;
            }
            if let Some(usage) = value.get("usage").filter(|v| !v.is_null()) {
                self.response.usage.prompt_tokens = usage["prompt_tokens"].as_u64().unwrap_or(0);
                self.response.usage.completion_tokens =
                    usage["completion_tokens"].as_u64().unwrap_or(0);
                read_cache_and_cost(usage, &mut self.response.usage);
            }
        }
        self.response.usage.total_tokens = self
            .response
            .usage
            .prompt_tokens
            .saturating_add(self.response.usage.completion_tokens);
        Ok(())
    }
    /// Compatible local servers often omit `index` after the first delta.
    /// Use an explicit index, then a matching call id, then continue one
    /// incomplete same-name call. A new name or a completed same-name call
    /// starts another slot. Ollama still treats each unindexed frame as a
    /// new call unless an id matches an earlier one.
    fn resolve_call_index(&self, call: &Value, position: usize, chunk_len: usize) -> usize {
        if let Some(index) = call["index"].as_u64() {
            return index as usize;
        }
        if let Some(id) = call["id"].as_str().filter(|id| !id.is_empty()) {
            if let Some((&index, _)) = self.calls.iter().find(|(_, part)| part.id == id) {
                return index;
            }
        }
        if self.ollama {
            return self.calls.len();
        }
        let name = call["function"]["name"].as_str().unwrap_or("");
        if name.is_empty() {
            return self.calls.keys().next_back().copied().unwrap_or(position);
        }
        let named: Vec<usize> = self
            .calls
            .iter()
            .filter(|(_, part)| part.name == name)
            .map(|(&index, _)| index)
            .collect();
        if chunk_len == 1 && named.len() == 1 {
            let args = &self.calls[&named[0]].args;
            if args.is_empty() || serde_json::from_str::<Value>(args).is_err() {
                return named[0];
            }
        }
        self.calls
            .keys()
            .next_back()
            .map(|index| index + 1)
            .unwrap_or(position)
    }
    fn full_response(&mut self, value: Value) -> Result<()> {
        self.observe_fields(&value);
        // An error object in a successful HTTP reply (some providers answer
        // out-of-credit or moderation errors this way) keeps its message.
        if self.ollama || value.get("error").is_some_and(|e| !e.is_null()) {
            self.chunk(value, &mut Vec::new())?;
        } else {
            let choice = value["choices"]
                .as_array()
                .and_then(|v| v.first())
                .context("Provider returned no completion choices")?;
            ensure!(
                choice["message"].is_object(),
                "Provider returned no assistant message"
            );
            let chunk = json!({"choices":[{"delta":choice["message"],"finish_reason":choice["finish_reason"]}],"usage":value["usage"],"timings":value["timings"]});
            self.chunk(chunk, &mut Vec::new())?;
        }
        Ok(())
    }
    pub fn finish(mut self) -> Result<ChatResponse> {
        if !self.seen_finish {
            return Err(anyhow::Error::new(
                crate::retry::ModelFailure::Disconnected {
                    message: "Model stream ended without a finish marker; no tools were executed"
                        .into(),
                },
            ));
        }
        ensure!(
            !matches!(
                self.response.finish_reason.as_str(),
                "length" | "max_tokens" | "content_filter"
            ),
            "Model response was cut short ({}); no partial tools were executed",
            self.response.finish_reason
        );
        ensure!(
            !self.duplicate_call_ids,
            "Model returned duplicate tool call IDs in one response; no calls from that response were executed"
        );
        // Provider IDs belong to one response's protocol group. Reuse in a
        // later response is valid, but two slots with one ID cannot be paired
        // with their results or recovered safely. Refuse the entire response
        // before the engine can execute any call; do not echo untrusted IDs.
        let mut ids = BTreeSet::new();
        for (_, part) in self.calls {
            ensure!(!part.name.is_empty(), "Tool call has no name");
            let arguments: Value = match serde_json::from_str::<Value>(if part.args.is_empty() {
                "{}"
            } else {
                &part.args
            }) {
                Ok(value) if value.is_object() => value,
                _ => {
                    // Almost JSON (a fence, trailing commas, single quotes,
                    // raw newlines, encoded twice): repaired, and noted.
                    let repaired = crate::tool_repair::arguments(&part.args)
                        .context("Model returned incomplete or invalid tool arguments")?;
                    self.response.repaired += 1;
                    repaired
                }
            };
            ensure!(arguments.is_object(), "Tool arguments must be an object");
            let id = if part.id.is_empty() {
                format!("call_{}", crate::id())
            } else {
                part.id
            };
            ensure!(
                ids.insert(id.clone()),
                "Model returned duplicate tool call IDs in one response; no calls from that response were executed"
            );
            self.response.tool_calls.push(ToolCall {
                id,
                name: part.name,
                arguments,
            });
        }
        Ok(self.response)
    }
}

/// Prompt-cache and cost fields from an OpenAI-style `usage` object:
/// `prompt_tokens_details.cached_tokens` (OpenAI, OpenRouter, recent
/// llama.cpp), `prompt_cache_hit_tokens` (DeepSeek), and OpenRouter's
/// `cost` (US dollars) and `prompt_tokens_details.cache_write_tokens`.
pub fn read_cache_and_cost(usage: &Value, into: &mut Usage) {
    let details = &usage["prompt_tokens_details"];
    into.cached_tokens = details["cached_tokens"]
        .as_u64()
        .or_else(|| usage["prompt_cache_hit_tokens"].as_u64())
        .unwrap_or(0);
    into.cache_write_tokens = details["cache_write_tokens"].as_u64().unwrap_or(0);
    if let Some(cost) = usage["cost"]
        .as_f64()
        .filter(|c| c.is_finite() && *c >= 0.0)
    {
        into.cost_usd = Some(cost);
    }
}

pub fn preset(provider: &str) -> Value {
    let (label, endpoint, key, local) = match provider {
        "mock" => ("Offline demo", "", "OPENAI_API_KEY", true),
        "ollama" => (
            "Ollama",
            "http://127.0.0.1:11434/v1",
            "OLLAMA_API_KEY",
            true,
        ),
        "local" => (
            "LM Studio / local",
            "http://127.0.0.1:1234/v1",
            "OPENAI_API_KEY",
            true,
        ),
        "llamacpp" => (
            "llama.cpp",
            "http://127.0.0.1:8080/v1",
            "OPENAI_API_KEY",
            true,
        ),
        "vllm" => ("vLLM", "http://127.0.0.1:8000/v1", "OPENAI_API_KEY", true),
        "grok" => ("Grok / xAI", "https://api.x.ai/v1", "XAI_API_KEY", false),
        _ => (
            "OpenAI-compatible",
            "https://api.openai.com/v1",
            "OPENAI_API_KEY",
            false,
        ),
    };
    json!({"id":provider,"provider":provider,"label":label,"name":label,"endpoint":endpoint,"api_key_env":key,"local":local,"needs_key":!local,"running":provider=="mock"})
}

pub fn presets() -> Vec<Value> {
    [
        "ollama",
        "local",
        "llamacpp",
        "vllm",
        "openai_compatible",
        "grok",
        "mock",
    ]
    .iter()
    .map(|p| preset(p))
    .collect()
}

pub async fn detect() -> Vec<Value> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(1200))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("TLS client");
    futures_util::future::join_all(["ollama","local","llamacpp","vllm"].into_iter().map(|provider|{
        let client=client.clone();async move {
            let preset=preset(provider);let base=preset["endpoint"].as_str().unwrap_or("");
            let url=if provider=="ollama"{format!("{}/api/tags",base.trim_end_matches("/v1"))}else{format!("{base}/models")};
            let start=std::time::Instant::now();
            let result: Result<Value> = async {
                let response = client.get(url).send().await?.error_for_status()?;
                let mut stream = response.bytes_stream();
                let mut bytes = Vec::new();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk?;
                    ensure!(bytes.len() + chunk.len() <= 2_000_000, "Model list exceeded 2 MB");
                    bytes.extend_from_slice(&chunk);
                }
                Ok(serde_json::from_slice(&bytes)?)
            }.await;
            match result {
                Ok(value)=>{
                    let empty=Vec::new();let rows=value[if provider=="ollama"{"models"}else{"data"}].as_array().unwrap_or(&empty);
                    let models: Vec<_> = rows
                        .iter()
                        .filter_map(|m| {
                            let name = if provider == "ollama" {
                                m["name"].as_str()
                            } else {
                                m["id"].as_str()
                            }?;
                            let mut caps = serde_json::Map::new();
                            caps.insert("completion".into(), Value::Bool(true));
                            if let Some(list) = m.get("capabilities").and_then(Value::as_array) {
                                for item in list {
                                    if let Some(s) = item.as_str() {
                                        caps.insert(s.to_string(), Value::Bool(true));
                                    }
                                }
                            }
                            Some(json!({
                                "id": name,
                                "name": name,
                                "size_bytes": m["size"].as_u64().unwrap_or(0),
                                "context_limit": m.pointer("/details/context_length").and_then(Value::as_u64).unwrap_or(0),
                                "capabilities": Value::Object(caps),
                                "detail": m.pointer("/details/parameter_size").and_then(Value::as_str).unwrap_or("")
                            }))
                        })
                        .collect();
                    json!({"provider":provider,"label":preset["label"],"endpoint":base,"running":true,"latency_ms":start.elapsed().as_millis(),"detail":format!("{} models available",models.len()),"models":models})
                }
                Err(_)=>json!({"provider":provider,"label":preset["label"],"endpoint":base,"running":false,"latency_ms":start.elapsed().as_millis(),"models":[],"detail":"Not reachable"}),
            }
        }
    })).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_errors_show_what_the_provider_said() {
        // OpenRouter's answer to a request the account cannot pay for.
        let body = br#"{"error":{"message":"This request requires more credits, or fewer max_tokens. You requested up to 32000 tokens, but can only afford 1133.","code":402}}"#;
        assert_eq!(
            provider_error_detail(body, false).as_deref(),
            Some("This request requires more credits, or fewer max_tokens. You requested up to 32000 tokens, but can only afford 1133.")
        );
        // A remote HTML error page is not dumped into the task.
        assert_eq!(
            provider_error_detail(b"<html>Bad gateway</html>", false),
            None
        );
        // A local runtime's plain-text explanation still is, bounded.
        let long = "context too long ".repeat(100);
        let detail = provider_error_detail(long.as_bytes(), true).unwrap();
        assert!(detail.starts_with("context too long"));
        assert!(detail.len() <= 600);
        assert_eq!(provider_error_detail(b"", true), None);
        // Keys in a provider message are redacted (built at run time so the
        // secret scanner does not flag a fixture).
        let key = format!("sk-or-v1-{}", "0123456789abcdef".repeat(4));
        let leaked = json!({"error": format!("bad key {key}")}).to_string();
        assert!(!provider_error_detail(leaked.as_bytes(), false)
            .unwrap_or_default()
            .contains(&key));
    }

    #[test]
    fn loopback_detection_and_llama_schema_rewrite() {
        assert!(is_loopback_endpoint("http://127.0.0.1:8080/v1"));
        assert!(is_loopback_endpoint("http://localhost:1234/v1"));
        assert!(is_loopback_endpoint("http://[::1]:9/v1"));
        assert!(!is_loopback_endpoint("https://api.openai.com/v1"));
        assert!(!is_loopback_endpoint("http://192.168.1.2:11434"));
        let mut schema = json!({"type":"object","properties":{"params":{"type":"array","items":{"type":["string","null"]}},"type":{"type":"string"}}});
        type_lists_to_any_of(&mut schema);
        assert_eq!(
            schema["properties"]["params"]["items"],
            json!({"anyOf":[{"type":"string"},{"type":"null"}]})
        );
        assert_eq!(schema["properties"]["type"], json!({"type":"string"}));
        assert_eq!(schema["type"], "object");
    }
}
