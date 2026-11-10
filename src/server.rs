//! Server-only code: app state, the Ollama client, the tool loop, and
//! reply streaming.

use crate::calc;
use crate::calculator::{CalcSource, Calculation};
use crate::chat::{Attachment, ChatEntry, ChatInfo, DEFAULT_FILE_PROMPT, Role, StreamEvent, ToolKind, ToolUse};
use crate::notes::{self, Note};
use actix_web::web;
use futures::{StreamExt, channel::mpsc};
use leptos::prelude::ServerFnError;
use leptos::server_fn::{Bytes, codec::ByteStream};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

/// Sent as the first message of every conversation. Never stored in history
/// or shown in the UI. Override with OLLAMA_SYSTEM_PROMPT.
const DEFAULT_SYSTEM_PROMPT: &str = "You are a helpful assistant inside a productivity app. \
Always reply in the same language as the user's latest message, and never switch \
languages or writing systems in the middle of a reply. Be clear and concise.";

/// Always appended to the system prompt, even a custom one, because the app
/// depends on it.
///
/// The "only when… otherwise answer directly" wording matters: a looser
/// version made mistral-nemo call the tool on 3 of 12 non-math questions
/// (copying the description's examples); this one measured 0 of 12, while
/// still calling it on 9 of 9 math questions.
const TOOL_INSTRUCTIONS: &str = "You have a `calculate` tool. Call it only when the \
user's question requires working out a number, and then call it for each value you need \
instead of doing arithmetic yourself, including in follow-up questions. For anything \
else, such as explanations, facts, advice or writing, answer directly and do not call \
any tool. Round money to 2 decimal places in your answer.

You can also use the user's notes. When the user mentions their notes, or asks about \
something they may have written down (plans, lists, ideas, meetings), call `search_notes`, \
then `read_note` on the relevant results, and answer from what the notes say, naming the \
note you used. If nothing relevant is found, say so instead of guessing. Only call \
`create_note` when the user explicitly asks you to save or write something down as a note.";

/// Longest note text handed to the model in one `read_note` call (about
/// 1.5k tokens), so a huge note can't crowd out the conversation.
const READ_NOTE_MAX_CHARS: usize = 6000;
/// Most notes returned by one `search_notes` call.
const SEARCH_RESULTS: usize = 8;

/// Used when OLLAMA_MODEL isn't set. On the app's benchmark (wage, tip,
/// follow-up, prime sums, non-math, language) it scored 12/14 with thinking
/// off versus 9/14 for mistral-nemo, and with thinking on it solved the
/// hardest question (first 100 non-primes) 2/2, which mistral-nemo never did.
const DEFAULT_MODEL: &str = "bonsai-2:27b";

/// Mistral recommends a low temperature for Mistral models; Ollama's default
/// (0.8) made mistral-nemo drift, e.g. switching languages mid-reply. Other
/// models keep the settings their makers tuned (thinking models in particular
/// get worse at low temperatures). Override with OLLAMA_TEMPERATURE.
const MISTRAL_TEMPERATURE: f32 = 0.3;

/// With thinking on, a model reasons before answering, which can take
/// thousands of tokens (bonsai-2:27b used 3.8k on one maths question), so
/// replies get more room and a larger context window.
const THINKING_NUM_PREDICT: i32 = 8192;
const THINKING_NUM_CTX: u32 = 12288;

/// Context window in tokens. Ollama defaults to 4096, which long chats and
/// attached files overflow; Ollama then silently drops the oldest messages.
/// 8192 adds roughly 1.3 GB of memory for mistral-nemo.
const DEFAULT_NUM_CTX: u32 = 8192;

/// Most tokens per model response. Without a cap, a response Ollama hides
/// from us (a tool call it failed to parse) can run until the context window
/// is full, which takes minutes and blocks every other request meanwhile.
const DEFAULT_NUM_PREDICT: i32 = 2048;

/// Most tool rounds per reply, so a confused model can't loop forever.
const MAX_TOOL_ROUNDS: usize = 4;

/// Added to the latest user message when retrying a round that came back
/// empty. Ollama 0.40 sometimes drops a tool call it can't parse, leaving no
/// text and no call; with this note mistral-nemo recovered in 3 of 3 tests.
/// Only sent with the retry, never stored or shown.
const EMPTY_REPLY_HINT: &str = "\n\n(Note: your previous reply to this message could not be read. \
If you call the calculate tool, send exactly one well-formed call whose expression has \
balanced parentheses; otherwise answer in plain text.)";

/// Oldest calculations are dropped beyond this.
const MAX_CALCULATIONS: usize = 1000;

// ======================================================================
// State
// ======================================================================

/// A message in the form Ollama expects.
#[derive(Clone, Serialize)]
struct ModelMessage {
    role: &'static str,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_name: Option<String>,
}

impl ModelMessage {
    fn new(role: &'static str, content: impl Into<String>) -> Self {
        Self { role, content: content.into(), tool_calls: None, tool_name: None }
    }
}

/// A history entry as the UI shows it, plus the messages the model saw for it
/// (full file text, tool calls and results), which never leave the server.
#[derive(Clone)]
struct StoredEntry {
    entry: ChatEntry,
    model: Vec<ModelMessage>,
}

pub struct AppState {
    pub db: crate::db::Db,
    /// For fetching news feeds (its own user agent and timeout).
    pub http: reqwest::Client,
    /// Held while news feeds refresh, so refreshes don't overlap.
    pub news_refresh: Mutex<()>,
    client: reqwest::Client,
    chat_url: String,
    model: String,
    system_prompt: String,
    /// None = use the model's own default.
    temperature: Option<f32>,
    /// Whether the model supports thinking (asked from Ollama at startup).
    can_think: bool,
    num_ctx: u32,
    num_predict: i32,
    history: Mutex<Vec<StoredEntry>>,
    calculations: Mutex<Vec<Calculation>>,
    next_calc_id: AtomicU64,
}

impl AppState {
    /// Reads settings from the environment and asks Ollama what the model
    /// can do (if Ollama isn't running yet, thinking is assumed unsupported).
    pub async fn new() -> Self {
        let env = |key: &str| std::env::var(key).ok().filter(|v| !v.trim().is_empty());
        let base = env("OLLAMA_URL").unwrap_or_else(|| "http://localhost:11434".into());
        let base = base.trim_end_matches('/').to_string();
        let model = env("OLLAMA_MODEL").unwrap_or_else(|| DEFAULT_MODEL.into());
        let temperature = match env("OLLAMA_TEMPERATURE").map(|t| t.parse::<f32>()) {
            Some(Ok(t)) if (0.0..=2.0).contains(&t) => Some(t),
            Some(_) => {
                eprintln!("Ignoring invalid OLLAMA_TEMPERATURE; using the default");
                None
            }
            None => None,
        }
        .or_else(|| model.to_lowercase().contains("mistral").then_some(MISTRAL_TEMPERATURE));
        let num_ctx = match env("OLLAMA_NUM_CTX").map(|n| n.parse::<u32>()) {
            Some(Ok(n)) if n >= 512 => n,
            Some(_) => {
                eprintln!("Ignoring invalid OLLAMA_NUM_CTX; using {DEFAULT_NUM_CTX}");
                DEFAULT_NUM_CTX
            }
            None => DEFAULT_NUM_CTX,
        };
        let num_predict = match env("OLLAMA_NUM_PREDICT").map(|n| n.parse::<i32>()) {
            Some(Ok(n)) if n >= 64 => n,
            Some(_) => {
                eprintln!("Ignoring invalid OLLAMA_NUM_PREDICT; using {DEFAULT_NUM_PREDICT}");
                DEFAULT_NUM_PREDICT
            }
            None => DEFAULT_NUM_PREDICT,
        };
        let base_prompt = env("OLLAMA_SYSTEM_PROMPT").unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.into());
        let client = reqwest::Client::new();
        let can_think = model_capabilities(&client, &base, &model).await.iter().any(|c| c == "thinking");
        println!("Model: {model} (thinking {})", if can_think { "supported" } else { "not supported" });
        let db = crate::db::Db::open_from_env().unwrap_or_else(|e| panic!("{e}"));
        Self {
            db,
            http: crate::feeds::client(),
            news_refresh: Mutex::new(()),
            client,
            chat_url: format!("{base}/api/chat"),
            model,
            system_prompt: format!("{base_prompt}\n\n{TOOL_INSTRUCTIONS}"),
            temperature,
            can_think,
            num_ctx,
            num_predict,
            history: Mutex::new(Vec::new()),
            calculations: Mutex::new(Vec::new()),
            next_calc_id: AtomicU64::new(1),
        }
    }

    pub async fn chat_info(&self) -> ChatInfo {
        let history = self.history.lock().await;
        ChatInfo {
            model: self.model.clone(),
            can_think: self.can_think,
            messages: history.iter().map(|s| s.entry.clone()).collect(),
        }
    }

    pub async fn clear_chat(&self) {
        self.history.lock().await.clear();
    }

    /// Newest first.
    pub async fn calculations(&self) -> Vec<Calculation> {
        self.calculations.lock().await.iter().rev().cloned().collect()
    }

    pub async fn record_calculation(
        &self,
        expression: String,
        result: String,
        source: CalcSource,
        context: Option<String>,
    ) -> Calculation {
        let calculation = Calculation {
            id: self.next_calc_id.fetch_add(1, Ordering::Relaxed),
            expression,
            result,
            source,
            context,
            at_ms: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
        };
        let mut list = self.calculations.lock().await;
        list.push(calculation.clone());
        if list.len() > MAX_CALCULATIONS {
            let excess = list.len() - MAX_CALCULATIONS;
            list.drain(..excess);
        }
        calculation
    }

    pub async fn clear_calculations(&self) {
        self.calculations.lock().await.clear();
    }
}

/// What Ollama says the model can do, e.g. ["completion", "tools", "thinking"].
async fn model_capabilities(client: &reqwest::Client, base: &str, model: &str) -> Vec<String> {
    #[derive(Deserialize)]
    struct Show {
        #[serde(default)]
        capabilities: Vec<String>,
    }
    let response = client.post(format!("{base}/api/show")).json(&json!({ "model": model })).send().await;
    match response {
        Ok(r) if r.status().is_success() => r.json::<Show>().await.map(|s| s.capabilities).unwrap_or_default(),
        Ok(r) => {
            eprintln!("Ollama couldn't describe model '{model}' ({}); is it pulled?", r.status());
            Vec::new()
        }
        Err(e) => {
            eprintln!("Couldn't reach Ollama at {base} to check the model: {e}");
            Vec::new()
        }
    }
}

/// Fetches the shared state inside a server function.
pub async fn state() -> Result<web::Data<AppState>, ServerFnError> {
    leptos_actix::extract::<web::Data<AppState>>()
        .await
        .map_err(|e| ServerFnError::new(format!("App state unavailable: {e}")))
}

// ======================================================================
// Ollama wire types
// ======================================================================

#[derive(Serialize)]
struct OllamaChatRequest<'a> {
    model: &'a str,
    messages: &'a [ModelMessage],
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a Value>,
    stream: bool,
    /// Only sent to models that support thinking.
    #[serde(skip_serializing_if = "Option::is_none")]
    think: Option<bool>,
    options: OllamaOptions,
}

#[derive(Serialize)]
struct OllamaOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    num_ctx: u32,
    num_predict: i32,
}

#[derive(Deserialize)]
struct OllamaChunk {
    message: Option<OllamaChunkMessage>,
    #[serde(default)]
    done: bool,
    /// "length" when the response hit num_predict.
    done_reason: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct OllamaChunkMessage {
    #[serde(default)]
    content: String,
    /// The model's private reasoning (thinking models only).
    #[serde(default)]
    thinking: String,
    #[serde(default)]
    tool_calls: Vec<Value>,
}

#[derive(Deserialize)]
struct OllamaError {
    error: String,
}

fn tool_definitions() -> Value {
    json!([{
        "type": "function",
        "function": {
            "name": "calculate",
            // Worked examples measurably improved mistral-nemo's expressions
            // (e.g. it stopped writing nonsense for percentage questions).
            "description": "Only for questions that need a calculation. \
                Evaluates one arithmetic expression exactly and returns the number. \
                Call it once per value you need. \
                Examples: 13 * 50 / 60 (pay for 50 minutes at 13 per hour); \
                84.50 * 15 / 100 (15% of 84.50); 84.50 * 1.15 (84.50 plus 15%); 13 * 37.5 * 4; \
                sum(prime(k), k, 1, 100) (sum of the first 100 primes); sum(k^2, k, 1, 10). \
                Supports + - * / ^, parentheses, comparisons (< > <= >= == !=), and functions \
                sqrt, round(x, digits), floor, ceil, abs, min, max, ln, log, gcd, lcm, \
                prime(k) (the k-th prime), isprime(x), if(condition, a, b), \
                sum(expression, k, from, to) and prod(expression, k, from, to). \
                Conditions can use true, false, and, or, not. \
                There are no other functions and no loops or programs. \
                Use . as the decimal separator and no thousands separators or currency symbols.",
            "parameters": {
                "type": "object",
                "properties": {
                    "expression": {
                        "type": "string",
                        "description": "e.g. 84.50 * 15 / 100"
                    }
                },
                "required": ["expression"]
            }
        }
    }, {
        "type": "function",
        "function": {
            "name": "search_notes",
            "description": "Searches the user's saved notes by words in the title or text and \
                returns the best matches with their id, title, when they were last edited and \
                a preview. Use a few keywords, e.g. \"groceries\" or \"project ideas\". \
                An empty query lists the most recently edited notes.",
            "parameters": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Keywords to look for" }
                },
                "required": ["query"]
            }
        }
    }, {
        "type": "function",
        "function": {
            "name": "read_note",
            "description": "Returns the full text of one of the user's notes, by the id that \
                search_notes returned.",
            "parameters": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "The note's id" }
                },
                "required": ["id"]
            }
        }
    }, {
        "type": "function",
        "function": {
            "name": "create_note",
            "description": "Saves a new note for the user. Only use this when the user asks \
                you to save, note down or write something as a note. Markdown is allowed in \
                the body.",
            "parameters": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "A short title" },
                    "body": { "type": "string", "description": "The note's text" }
                },
                "required": ["title", "body"]
            }
        }
    }])
}

// ======================================================================
// Streaming
// ======================================================================

/// Starts generating a reply and returns it as a stream of newline-delimited
/// JSON [`StreamEvent`]s. The exchange is saved to history once the model
/// finishes. If the browser disconnects, generation stops and nothing is saved.
pub fn stream_reply(
    state: web::Data<AppState>,
    message: String,
    attachment: Option<Attachment>,
    think: bool,
) -> ByteStream {
    let question = if message.trim().is_empty() {
        DEFAULT_FILE_PROMPT.to_string()
    } else {
        message.trim().to_string()
    };

    let (model_text, attachment_name) = match attachment {
        Some(Attachment { name, content }) => {
            (format!("File `{name}`:\n```\n{content}\n```\n\n{question}"), Some(name))
        }
        None => (question.clone(), None),
    };
    let user = StoredEntry {
        model: vec![ModelMessage::new("user", model_text)],
        entry: ChatEntry { role: Role::User, content: question, attachment: attachment_name, tools: Vec::new(), thinking: None },
    };

    let (tx, rx) = mpsc::unbounded::<Bytes>();
    let think = think && state.can_think;
    actix_web::rt::spawn(run_chat(state, user, think, tx));
    ByteStream::new(rx.map(Ok::<_, ServerFnError>))
}

fn encode(event: &StreamEvent) -> Bytes {
    let mut line = serde_json::to_vec(event).expect("StreamEvent always serializes");
    line.push(b'\n');
    Bytes::from(line)
}

/// Why a round stopped early.
enum Stop {
    /// The browser went away; stop quietly.
    Disconnected,
    Failed(String),
}

async fn run_chat(state: web::Data<AppState>, user: StoredEntry, think: bool, tx: mpsc::UnboundedSender<Bytes>) {
    // Returns false once the browser has gone away.
    let send = |event: StreamEvent| tx.unbounded_send(encode(&event)).is_ok();

    // Snapshot the history so the lock isn't held while the model works.
    let mut messages = vec![ModelMessage::new("system", state.system_prompt.clone())];
    messages.extend(state.history.lock().await.iter().flat_map(|s| s.model.iter().cloned()));
    messages.extend(user.model.iter().cloned());

    let tools = tool_definitions();
    let context = Some(truncate(&user.entry.content, 90));
    let mut produced: Vec<ModelMessage> = Vec::new(); // this turn's model-side messages
    let mut tools_used: Vec<ToolUse> = Vec::new();
    let mut reply = String::new(); // everything shown to the user
    let mut thoughts = String::new(); // the model's reasoning, if thinking

    for round in 0..=MAX_TOOL_ROUNDS {
        // The last round offers no tools, which forces a written answer.
        let offer_tools = round < MAX_TOOL_ROUNDS;
        let mut attempt = 0;
        let (content, calls) = loop {
            // On the retry, the latest user message carries EMPTY_REPLY_HINT.
            let hinted;
            let request: &[ModelMessage] = if attempt == 0 {
                &messages
            } else {
                hinted = with_hint(&messages);
                &hinted
            };
            let result = match stream_round(&state, request, offer_tools.then_some(&tools), think, &send, &mut reply, &mut thoughts).await {
                Ok(r) => r,
                Err(Stop::Disconnected) => return,
                Err(Stop::Failed(message)) => {
                    send(StreamEvent::Error { message });
                    return;
                }
            };
            let empty = result.0.trim().is_empty() && result.1.is_empty();
            if empty && attempt == 0 {
                attempt += 1;
                continue;
            }
            break result;
        };

        if calls.is_empty() || !offer_tools {
            if reply.trim().is_empty() {
                // Never finish silently: say what happened instead of an empty card.
                let message = if tools_used.is_empty() {
                    "The model returned an empty answer. Try asking again or rephrasing."
                } else {
                    "The model used the calculator but didn't write an answer. Try asking again or rephrasing."
                };
                send(StreamEvent::Error { message: message.into() });
                return;
            }
            produced.push(ModelMessage::new("assistant", content));
            let mut history = state.history.lock().await;
            history.push(user);
            history.push(StoredEntry {
                entry: ChatEntry {
                    role: Role::Assistant,
                    content: reply,
                    attachment: None,
                    tools: tools_used,
                    thinking: (!thoughts.trim().is_empty()).then_some(thoughts),
                },
                model: produced,
            });
            drop(history);
            send(StreamEvent::Done);
            return;
        }

        let call_message = ModelMessage { tool_calls: Some(calls.clone()), ..ModelMessage::new("assistant", content) };
        messages.push(call_message.clone());
        produced.push(call_message);

        for call in &calls {
            let (name, result_for_model, tool_use) = run_tool(&state, call, context.clone()).await;
            if let Some(t) = &tool_use {
                if !send(StreamEvent::Tool { tool: t.clone() }) {
                    return;
                }
            }
            tools_used.extend(tool_use);
            let result = ModelMessage { tool_name: Some(name), ..ModelMessage::new("tool", result_for_model) };
            messages.push(result.clone());
            produced.push(result);
        }
    }
}

/// Streams one model response. Text is forwarded to the browser as it
/// arrives (and appended to `reply`); tool calls are collected and returned.
async fn stream_round(
    state: &AppState,
    messages: &[ModelMessage],
    tools: Option<&Value>,
    think: bool,
    send: &impl Fn(StreamEvent) -> bool,
    reply: &mut String,
    thoughts: &mut String,
) -> Result<(String, Vec<Value>), Stop> {
    let (num_ctx, num_predict) = if think {
        (state.num_ctx.max(THINKING_NUM_CTX), state.num_predict.max(THINKING_NUM_PREDICT))
    } else {
        (state.num_ctx, state.num_predict)
    };
    let body = OllamaChatRequest {
        model: &state.model,
        messages,
        tools,
        stream: true,
        think: state.can_think.then_some(think),
        options: OllamaOptions { temperature: state.temperature, num_ctx, num_predict },
    };

    let response = state
        .client
        .post(&state.chat_url)
        .json(&body)
        .send()
        .await
        .map_err(|e| Stop::Failed(format!("Could not reach Ollama at {}: {e}", state.chat_url)))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        let detail = serde_json::from_str::<OllamaError>(&text).map(|e| e.error).unwrap_or(text);
        return Err(Stop::Failed(format!("Ollama returned {status}: {detail}")));
    }

    let mut chunks = response.bytes_stream();
    let mut buffer: Vec<u8> = Vec::new();
    let mut content = String::new();
    let mut calls: Vec<Value> = Vec::new();
    let mut first_text = true;

    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|e| Stop::Failed(format!("Lost connection to Ollama: {e}")))?;
        buffer.extend_from_slice(&chunk);

        // Ollama sends one JSON object per line; a line may span several chunks.
        while let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buffer.drain(..=pos).collect();
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let parsed: OllamaChunk = serde_json::from_slice(&line)
                .map_err(|e| Stop::Failed(format!("Unexpected response from Ollama: {e}")))?;
            if let Some(error) = parsed.error {
                return Err(Stop::Failed(format!("Ollama error: {error}")));
            }
            if let Some(message) = parsed.message {
                calls.extend(message.tool_calls);
                if !message.thinking.is_empty() {
                    thoughts.push_str(&message.thinking);
                    if !send(StreamEvent::Thinking { text: message.thinking }) {
                        return Err(Stop::Disconnected);
                    }
                }
                if !message.content.is_empty() {
                    let mut text = message.content;
                    // Separate text written after a tool call from text before it.
                    if first_text && !reply.is_empty() && !reply.ends_with(char::is_whitespace) {
                        text.insert_str(0, "\n\n");
                    }
                    first_text = false;
                    content.push_str(&text);
                    reply.push_str(&text);
                    if !send(StreamEvent::Token { text }) {
                        return Err(Stop::Disconnected); // dropping the request stops generation
                    }
                }
            }
            if parsed.done {
                if parsed.done_reason.as_deref() == Some("length") && content.trim().is_empty() && calls.is_empty() {
                    return Err(Stop::Failed(if think {
                        "The model ran out of room while thinking. Try a simpler question, or turn off Think.".into()
                    } else {
                        "The model's reply was too long and was cut off.".into()
                    }));
                }
                return Ok((content, calls));
            }
        }
    }
    Err(Stop::Failed("Ollama closed the connection before finishing the reply.".into()))
}

/// Runs one tool call. Returns the tool name, the text the model sees, and
/// what to show in the UI.
async fn run_tool(state: &AppState, call: &Value, context: Option<String>) -> (String, String, Option<ToolUse>) {
    let function = &call["function"];
    let name = function["name"].as_str().unwrap_or_default().to_string();
    // Arguments normally arrive as an object, but some models send a JSON string.
    let args = match &function["arguments"] {
        Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
        other => other.clone(),
    };
    let (for_model, shown) = match name.as_str() {
        "calculate" => tool_calculate(state, &args, context).await,
        "search_notes" => tool_search_notes(state, &args).await,
        "read_note" => tool_read_note(state, &args).await,
        "create_note" => tool_create_note(state, &args).await,
        _ => (
            format!("Error: there is no tool called '{name}'. The tools are calculate, search_notes, read_note and create_note."),
            Some(ToolUse { tool: ToolKind::Unknown, input: name.clone(), output: "Not an available tool".into(), ok: false, link: None }),
        ),
    };
    (name, for_model, shown)
}

fn tool_use(tool: ToolKind, input: impl Into<String>, output: impl Into<String>, ok: bool, link: Option<String>) -> Option<ToolUse> {
    Some(ToolUse { tool, input: input.into(), output: output.into(), ok, link })
}

async fn tool_calculate(state: &AppState, args: &Value, context: Option<String>) -> (String, Option<ToolUse>) {
    let Some(expression) = args["expression"].as_str().map(str::trim).filter(|e| !e.is_empty()) else {
        return ("Error: missing the `expression` argument, e.g. {\"expression\": \"13 * 50 / 60\"}.".into(), None);
    };
    match calc::evaluate_to_string(expression) {
        Ok(result) => {
            state.record_calculation(expression.to_string(), result.clone(), CalcSource::Assistant, context).await;
            (result.clone(), tool_use(ToolKind::Calculate, expression, result, true, None))
        }
        Err(e) => (
            format!("Error: {e}. Fix the expression and call the tool again."),
            tool_use(ToolKind::Calculate, expression, e.to_string(), false, None),
        ),
    }
}

async fn tool_search_notes(state: &AppState, args: &Value) -> (String, Option<ToolUse>) {
    let query = args["query"].as_str().unwrap_or_default().trim().to_string();
    let notes = match state.db.all_notes().await {
        Ok(n) => n,
        Err(e) => return (format!("Error: {e}"), tool_use(ToolKind::SearchNotes, query, "failed", false, None)),
    };
    let terms = notes::search_terms(&query);
    let mut hits: Vec<(u32, Note)> = if terms.is_empty() {
        notes.into_iter().map(|n| (0, n)).collect() // already newest first
    } else {
        let mut scored: Vec<(u32, Note)> = notes
            .into_iter()
            .map(|n| (notes::relevance(&terms, &n.title, &n.body), n))
            .filter(|(score, _)| *score > 0)
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.updated_ms.cmp(&a.1.updated_ms)));
        scored
    };
    hits.truncate(SEARCH_RESULTS);

    let summary = match hits.len() {
        0 => "no matches".to_string(),
        1 => "1 found".to_string(),
        n => format!("{n} found"),
    };
    let for_model = if hits.is_empty() {
        if terms.is_empty() { "The user has no notes yet.".into() } else { format!("No notes match \"{query}\".") }
    } else {
        let mut out = format!("{} (most relevant first):\n", if terms.is_empty() { "Most recent notes" } else { "Matching notes" });
        for (_, n) in &hits {
            out.push_str(&format!(
                "- id {}: \"{}\" (edited {}): {}\n",
                n.id,
                if n.title.trim().is_empty() { "Untitled" } else { n.title.trim() },
                ago(n.updated_ms),
                notes::snippet(&n.body),
            ));
        }
        out.push_str("Use read_note with an id to see a note's full text.");
        out
    };
    let shown_query = if query.is_empty() { "recent".to_string() } else { query };
    (for_model, tool_use(ToolKind::SearchNotes, shown_query, summary, true, None))
}

async fn tool_read_note(state: &AppState, args: &Value) -> (String, Option<ToolUse>) {
    // Models send ids as numbers or as strings.
    let id = args["id"].as_i64().or_else(|| args["id"].as_str().and_then(|s| s.trim().parse().ok()));
    let Some(id) = id else {
        return ("Error: missing the note `id` (a number from search_notes).".into(), None);
    };
    match state.db.get_note(id).await {
        Ok(Some(n)) => {
            let title = if n.title.trim().is_empty() { "Untitled".to_string() } else { n.title.trim().to_string() };
            let mut body: String = n.body.chars().take(READ_NOTE_MAX_CHARS).collect();
            if n.body.chars().count() > READ_NOTE_MAX_CHARS {
                body.push_str("\n[… the rest of this note was cut off because it is long]");
            }
            let for_model = format!("Note id {id}: \"{title}\" (edited {})\n\n{body}", ago(n.updated_ms));
            (for_model, tool_use(ToolKind::ReadNote, title, "", true, Some(format!("/notes/{id}"))))
        }
        Ok(None) => (
            format!("Error: there is no note with id {id}. Use search_notes to find the right id."),
            tool_use(ToolKind::ReadNote, format!("#{id}"), "not found", false, None),
        ),
        Err(e) => (format!("Error: {e}"), tool_use(ToolKind::ReadNote, format!("#{id}"), "failed", false, None)),
    }
}

async fn tool_create_note(state: &AppState, args: &Value) -> (String, Option<ToolUse>) {
    let title = args["title"].as_str().unwrap_or_default().trim().to_string();
    let body = args["body"].as_str().unwrap_or_default().to_string();
    if title.is_empty() && body.trim().is_empty() {
        return ("Error: give the note a `title` and a `body`.".into(), None);
    }
    let created = match state.db.create_note().await {
        Ok(n) => n,
        Err(e) => return (format!("Error: {e}"), tool_use(ToolKind::CreateNote, title, "failed", false, None)),
    };
    match state.db.save_note(created.id, title.clone(), body).await {
        Ok(_) => {
            let shown = if title.is_empty() { "Untitled".to_string() } else { title };
            (
                format!("Saved as a new note (id {}) titled \"{shown}\".", created.id),
                tool_use(ToolKind::CreateNote, shown, "", true, Some(format!("/notes/{}", created.id))),
            )
        }
        Err(e) => (format!("Error: {e}"), tool_use(ToolKind::CreateNote, title, "failed", false, None)),
    }
}

/// "today", "yesterday", "5 days ago": enough for the model to judge recency.
fn ago(ms: i64) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    match (now - ms).max(0) / 86_400_000 {
        0 => "today".into(),
        1 => "yesterday".into(),
        d => format!("{d} days ago"),
    }
}

/// A copy of `messages` with EMPTY_REPLY_HINT added to the latest user message.
fn with_hint(messages: &[ModelMessage]) -> Vec<ModelMessage> {
    let mut copy = messages.to_vec();
    if let Some(last_user) = copy.iter_mut().rev().find(|m| m.role == "user") {
        last_user.content.push_str(EMPTY_REPLY_HINT);
    }
    copy
}

fn truncate(s: &str, max_chars: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max_chars {
        s
    } else {
        format!("{}…", s.chars().take(max_chars - 1).collect::<String>().trim_end())
    }
}
