//! The Chat module: talks to the local model, with streaming replies,
//! file attachments, and the calculator tool.

use crate::{emoji, markdown};
use futures::StreamExt;
use leptos::html;
use leptos::prelude::*;
use leptos::server_fn::codec::{ByteStream, Json, Streaming};
use leptos::task::spawn_local;
use leptos_meta::Title;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use wasm_bindgen::JsCast;

pub const MAX_UPLOAD_BYTES: usize = 2 * 1024 * 1024;
pub const DEFAULT_FILE_PROMPT: &str = "Summarize this file.";

const SUGGESTIONS: [(&str, &str); 3] = [
    ("How much do I earn in 50 minutes at 13 € an hour?", "How much do I earn in 50 minutes at 13 € an hour?"),
    ("Write a haiku about compilers", "Write a haiku about compilers."),
    ("Weekend project ideas", "Give me 5 ideas for a weekend side project."),
];

// ======================================================================
// Types shared by server and browser
// ======================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// A calculator call the model made while answering.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolUse {
    pub tool: ToolKind,
    /// What the tool was given: an expression, a search, a note title.
    pub input: String,
    /// The result, or the error message if `ok` is false.
    pub output: String,
    pub ok: bool,
    /// Where the chip links to (e.g. the note that was read or created).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Calculate,
    SearchNotes,
    ReadNote,
    CreateNote,
    NewsHeadlines,
    ReadArticle,
    CalendarEvents,
    AddEvent,
    /// A tool name the model made up.
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatEntry {
    pub role: Role,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolUse>,
    /// The model's reasoning, when it was asked to think.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatInfo {
    pub model: String,
    /// Whether the model supports thinking (shows the Think toggle).
    pub can_think: bool,
    pub messages: Vec<ChatEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub content: String,
}

/// One line of the reply stream (newline-delimited JSON).
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    Token { text: String },
    Thinking { text: String },
    Tool { tool: ToolUse },
    Error { message: String },
    Done,
}

// ======================================================================
// Server functions
// ======================================================================

#[server]
pub async fn get_chat() -> Result<ChatInfo, ServerFnError> {
    Ok(crate::server::state().await?.chat_info().await)
}

#[server]
pub async fn clear_chat() -> Result<(), ServerFnError> {
    crate::server::state().await?.clear_chat().await;
    Ok(())
}

#[server(input = Json, output = Streaming)]
pub async fn send_message(
    message: String,
    attachment: Option<Attachment>,
    #[server(default)] think: bool,
) -> Result<ByteStream, ServerFnError> {
    if message.trim().is_empty() && attachment.is_none() {
        return Err(ServerFnError::new("Message is empty"));
    }
    if attachment.as_ref().is_some_and(|a| a.content.len() > MAX_UPLOAD_BYTES) {
        return Err(ServerFnError::new("Files must be 2 MB or smaller"));
    }
    let state = crate::server::state().await?;
    Ok(crate::server::stream_reply(state, message, attachment, think))
}

// ======================================================================
// UI state (lives at app level so a reply keeps streaming while you're
// on another tab)
// ======================================================================

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    User,
    Assistant,
    Error,
}

#[derive(Clone)]
struct UiMessage {
    id: usize,
    kind: Kind,
    content: ArcRwSignal<String>,
    attachment: Option<String>,
    tools: ArcRwSignal<Vec<ToolUse>>,
    thinking: ArcRwSignal<String>,
    streaming: ArcRwSignal<bool>,
}

#[derive(Clone)]
struct PendingFile {
    name: String,
    size: usize,
    content: String,
}

#[derive(Clone, Copy)]
pub struct Chat {
    messages: RwSignal<Vec<UiMessage>>,
    next_id: StoredValue<usize>,
    input: RwSignal<String>,
    file: RwSignal<Option<PendingFile>>,
    pending: RwSignal<bool>,
    /// Model supports thinking (set once chat info loads).
    can_think: RwSignal<bool>,
    /// The Think toggle.
    think: RwSignal<bool>,
    textarea: NodeRef<html::Textarea>,
    scroller: NodeRef<html::Main>,
}

impl Chat {
    pub fn new() -> Self {
        Self {
            messages: RwSignal::new(Vec::new()),
            next_id: StoredValue::new(0),
            input: RwSignal::new(String::new()),
            file: RwSignal::new(None),
            pending: RwSignal::new(false),
            can_think: RwSignal::new(false),
            think: RwSignal::new(false),
            textarea: NodeRef::new(),
            scroller: NodeRef::new(),
        }
    }

    fn push(&self, kind: Kind, content: String, attachment: Option<String>) -> UiMessage {
        let id = self.next_id.get_value();
        self.next_id.set_value(id + 1);
        let message = UiMessage {
            id,
            kind,
            content: ArcRwSignal::new(content),
            attachment,
            tools: ArcRwSignal::new(Vec::new()),
            thinking: ArcRwSignal::new(String::new()),
            streaming: ArcRwSignal::new(false),
        };
        self.messages.update(|m| m.push(message.clone()));
        message
    }

    fn load(&self, entries: Vec<ChatEntry>) {
        let start = self.next_id.get_value();
        let messages = entries
            .into_iter()
            .enumerate()
            .map(|(i, e)| UiMessage {
                id: start + i,
                kind: match e.role {
                    Role::User => Kind::User,
                    Role::Assistant => Kind::Assistant,
                },
                content: ArcRwSignal::new(e.content),
                attachment: e.attachment,
                tools: ArcRwSignal::new(e.tools),
                thinking: ArcRwSignal::new(e.thinking.unwrap_or_default()),
                streaming: ArcRwSignal::new(false),
            })
            .collect::<Vec<_>>();
        self.next_id.set_value(start + messages.len());
        self.messages.set(messages);
    }

    fn can_send(&self) -> bool {
        !self.pending.get() && (!self.input.with(|s| s.trim().is_empty()) || self.file.with(Option::is_some))
    }

    fn send(&self) {
        let chat = *self;
        if chat.pending.get_untracked() {
            return;
        }
        let text = chat.input.get_untracked();
        let attached = chat.file.get_untracked();
        if text.trim().is_empty() && attached.is_none() {
            return;
        }

        chat.pending.set(true);
        chat.input.set(String::new());
        chat.file.set(None);
        autosize(chat.textarea);

        let shown = if text.trim().is_empty() { DEFAULT_FILE_PROMPT.to_string() } else { text.trim().to_string() };
        chat.push(Kind::User, shown, attached.as_ref().map(|f| f.name.clone()));
        let reply = chat.push(Kind::Assistant, String::new(), None);
        reply.streaming.set(true);
        scroll_to_bottom(chat.scroller, true);

        let attachment = attached.map(|f| Attachment { name: f.name, content: f.content });
        let think = chat.can_think.get_untracked() && chat.think.get_untracked();
        spawn_local(async move {
            let result = stream_reply(text, attachment, think, &reply, || scroll_to_bottom(chat.scroller, false)).await;
            reply.streaming.set(false);
            if let Err(error) = result {
                let empty = reply.content.with_untracked(String::is_empty)
                    && reply.tools.with_untracked(Vec::is_empty)
                    && reply.thinking.with_untracked(String::is_empty);
                if empty {
                    chat.messages.update(|m| m.retain(|x| x.id != reply.id));
                }
                chat.push(Kind::Error, error, None);
                scroll_to_bottom(chat.scroller, false);
            }
            chat.pending.set(false);
            focus(chat.textarea);
        });
    }

    fn attach(&self, file: web_sys::File) {
        let chat = *self;
        spawn_local(async move {
            match read_text_file(file).await {
                Ok(f) => chat.file.set(Some(f)),
                Err(e) => {
                    chat.push(Kind::Error, e, None);
                    scroll_to_bottom(chat.scroller, true);
                }
            }
        });
    }

    fn new_chat(&self) {
        if self.pending.get_untracked() {
            return;
        }
        self.messages.set(Vec::new());
        spawn_local(async {
            let _ = clear_chat().await;
        });
        focus(self.textarea);
    }
}

impl Default for Chat {
    fn default() -> Self {
        Self::new()
    }
}

/// Reads the reply stream into `reply` as events arrive.
async fn stream_reply(
    message: String,
    attachment: Option<Attachment>,
    think: bool,
    reply: &UiMessage,
    on_progress: impl Fn(),
) -> Result<(), String> {
    let mut stream = send_message(message, attachment, think).await.map_err(error_text)?.into_inner();
    let mut buffer: Vec<u8> = Vec::new();

    while let Some(chunk) = stream.next().await {
        buffer.extend_from_slice(&chunk.map_err(error_text)?);
        // Decode only whole lines, so multi-byte characters split across
        // network chunks are never cut in half.
        while let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buffer.drain(..=pos).collect();
            match serde_json::from_slice::<StreamEvent>(&line) {
                Ok(StreamEvent::Token { text }) => {
                    reply.content.update(|c| c.push_str(&text));
                    on_progress();
                }
                Ok(StreamEvent::Thinking { text }) => {
                    reply.thinking.update(|t| t.push_str(&text));
                }
                Ok(StreamEvent::Tool { tool }) => {
                    reply.tools.update(|t| t.push(tool));
                    on_progress();
                }
                Ok(StreamEvent::Error { message }) => return Err(message),
                Ok(StreamEvent::Done) => return Ok(()),
                Err(e) => return Err(format!("Unexpected data from the server: {e}")),
            }
        }
    }
    Err("The connection closed before the reply finished.".into())
}

pub fn error_text(e: ServerFnError) -> String {
    match e {
        ServerFnError::ServerError(msg) => msg,
        other => other.to_string(),
    }
}

// ======================================================================
// Components
// ======================================================================

#[component]
pub fn ChatPage() -> impl IntoView {
    let chat = expect_context::<Chat>();
    let info = Resource::new(|| (), |_| get_chat());

    view! {
        <Title text="Chat · Workspace" />
        <section class="pane">
            <div class="toolbar">
                <button class="aero-btn" disabled=move || chat.pending.get() on:click=move |_| chat.new_chat()>
                    <span class="btn-icon icon-new" aria-hidden="true"></span>
                    "New chat"
                </button>
                <span class="spacer"></span>
                <Suspense fallback=|| ()>
                    {move || Suspend::new(async move {
                        info.await.ok().map(|i| view! {
                            <span class="model-pill" title="Model">
                                <span class="status-dot" aria-hidden="true"></span>
                                {i.model}
                            </span>
                        })
                    })}
                </Suspense>
            </div>

            <main class="scroller" node_ref=chat.scroller>
                <div class="thread" on:click=on_thread_click>
                    <Suspense fallback=|| ()>
                        {move || Suspend::new(async move {
                            // Seed from the server once. On the first load this
                            // happens during server rendering, so history arrives
                            // as HTML rather than after a second request.
                            if let Ok(i) = info.await {
                                chat.can_think.set(i.can_think);
                                if chat.messages.with_untracked(Vec::is_empty) {
                                    chat.load(i.messages);
                                }
                            }
                            view! { <Thread /> }
                        })}
                    </Suspense>
                </div>
            </main>

            <Composer />
        </section>
    }
}

#[component]
fn Thread() -> impl IntoView {
    let chat = expect_context::<Chat>();
    view! {
        <Show when=move || chat.messages.with(Vec::is_empty)>
            <EmptyState />
        </Show>
        <For each=move || chat.messages.get() key=|m| m.id children=|m| view! { <Message message=m /> } />
    }
}

#[component]
fn EmptyState() -> impl IntoView {
    let chat = expect_context::<Chat>();
    view! {
        <div class="empty">
            <div class="hero-orb" aria-hidden="true"></div>
            <h2>"How can I help you today?"</h2>
            <p>"Ask anything, or attach a text file to ask about it."</p>
            <div class="suggestions">
                {SUGGESTIONS
                    .iter()
                    .map(|&(label, prompt)| {
                        view! {
                            <button
                                type="button"
                                class="command-link"
                                on:click=move |_| {
                                    chat.input.set(prompt.to_string());
                                    chat.send();
                                }
                            >
                                <span class="arrow" aria-hidden="true">"➜"</span>
                                <span>{label}</span>
                            </button>
                        }
                    })
                    .collect_view()}
            </div>
        </div>
    }
}

#[component]
fn Message(message: UiMessage) -> impl IntoView {
    let content = RwSignal::from(message.content);
    let streaming = RwSignal::from(message.streaming);
    let tools = RwSignal::from(message.tools);
    let thinking = RwSignal::from(message.thinking);
    let attachment = message
        .attachment
        .map(|name| view! { <div class="attachment"><span class="emoji">"📄"</span> {name}</div> });
    // Short emoji-only messages ("👍", "😂🎉") are shown large, chat-app style.
    let jumbo = move || content.with(|c| emoji::is_emoji_only(c, 3));

    match message.kind {
        Kind::User => view! {
            <div class="msg user" class:jumbo=jumbo>
                {attachment}
                <div class="bubble">{move || content.with(|c| text_with_emoji(c))}</div>
            </div>
        }
        .into_any(),
        Kind::Error => view! {
            <div class="msg error" role="alert">
                <span class="error-icon" aria-hidden="true">"!"</span>
                <div class="bubble">{move || content.get()}</div>
            </div>
        }
        .into_any(),
        Kind::Assistant => view! {
            <div class="msg assistant">
                <span class="avatar" aria-hidden="true"></span>
                <div class="msg-body">
                    <div class="role">
                        <span>"Assistant"</span>
                        <Show when=move || !streaming.get() && content.with(|c| !c.is_empty())>
                            <button
                                type="button"
                                class="copy-msg"
                                on:click=move |ev| copy_text(content.get_untracked(), event_target(&ev))
                            >
                                "Copy"
                            </button>
                        </Show>
                    </div>
                    <Show when=move || thinking.with(|t| !t.is_empty())>
                        <ThinkingPanel
                            thinking
                            streaming
                            answered=Signal::derive(move || content.with(|c| !c.is_empty()) || tools.with(|t| !t.is_empty()))
                        />
                    </Show>
                    <Show when=move || tools.with(|t| !t.is_empty())>
                        <div class="tool-uses">
                            {move || tools.get().into_iter().map(|t| view! { <ToolChip tool=t /> }).collect_view()}
                        </div>
                    </Show>
                    // No empty card if a reply ends without text (an error follows instead).
                    <Show when=move || (streaming.get() && thinking.with(String::is_empty)) || content.with(|c| !c.is_empty())>
                        <div class="card" class:jumbo=jumbo>
                            <Show
                                when=move || streaming.get() && content.with(String::is_empty) && thinking.with(String::is_empty)
                                fallback=move || view! { <div class="md" inner_html=move || markdown::render(&content.get())></div> }
                            >
                                <div class="typing"><span></span><span></span><span></span></div>
                            </Show>
                        </div>
                    </Show>
                </div>
            </div>
        }
        .into_any(),
    }
}

/// Collapsible reasoning. While the model is still thinking it shows live
/// progress; afterwards "Thought for N words", expandable.
#[component]
fn ThinkingPanel(thinking: RwSignal<String>, streaming: RwSignal<bool>, answered: Signal<bool>) -> impl IntoView {
    let words = move || thinking.with(|t| t.split_whitespace().count());
    let in_progress = move || streaming.get() && !answered.get();
    view! {
        <details class="thinking" class:active=in_progress>
            <summary>
                <span class="thinking-orb" aria-hidden="true"></span>
                {move || if in_progress() {
                    format!("Thinking… {} words so far", words())
                } else {
                    format!("Thought for {} words", words())
                }}
            </summary>
            <div class="thinking-text">{move || thinking.get()}</div>
        </details>
    }
}

/// A chip above a reply showing a tool the model used, linking to where
/// its result lives (the Calculator history, or a note).
#[component]
fn ToolChip(tool: ToolUse) -> impl IntoView {
    let ok = tool.ok;
    let (label, icon, connector, default_link, title) = match tool.tool {
        ToolKind::Calculate => (
            "Calculator",
            "tool-icon",
            if ok { "=" } else { "→" },
            "/calculator",
            if ok {
                "Calculated exactly with the calculator tool. Open the Calculator tab to see the history."
            } else {
                "The model sent the calculator an expression it couldn't evaluate."
            },
        ),
        ToolKind::SearchNotes => ("Searched notes", "tool-icon-notes", "→", "/notes", "The model searched your notes."),
        ToolKind::ReadNote => ("Read note", "tool-icon-notes", "", "/notes", "The model read this note. Click to open it."),
        ToolKind::CreateNote => ("Created note", "tool-icon-notes", "", "/notes", "The model saved this as a new note. Click to open it."),
        ToolKind::NewsHeadlines => ("News", "tool-icon-news", "→", "/news", "The model looked at your news feeds."),
        ToolKind::ReadArticle => ("Read article", "tool-icon-news", "", "/news", "The model read this article. Click to open it."),
        ToolKind::CalendarEvents => ("Calendar", "tool-icon-calendar", "→", "/calendar", "The model looked at your calendar."),
        ToolKind::AddEvent => ("Added event", "tool-icon-calendar", "", "/calendar", "The model added this to your calendar. Click to open that day."),
        ToolKind::Unknown => ("Unknown tool", "tool-icon", "→", "/", "The model tried to use a tool that doesn't exist."),
    };
    let href = tool.link.clone().unwrap_or_else(|| default_link.to_string());
    let is_calc = tool.tool == ToolKind::Calculate;
    view! {
        <a class="tool-use" class:failed=!ok href=href title=title>
            <span class=icon aria-hidden="true"></span>
            <span class="tool-label">{label}</span>
            {(!tool.input.is_empty()).then(|| if is_calc {
                view! { <code class="tool-expr">{tool.input.clone()}</code> }.into_any()
            } else {
                view! { <span class="tool-input">"“" {tool.input.clone()} "”"</span> }.into_any()
            })}
            {(!connector.is_empty()).then(|| view! { <span class="tool-eq">{connector}</span> })}
            {(!tool.output.is_empty()).then(|| view! { <strong class="tool-out">{tool.output.clone()}</strong> })}
        </a>
    }
}

/// Plain text with each emoji wrapped in `<span class="emoji">`.
fn text_with_emoji(s: &str) -> impl IntoView + use<> {
    emoji::segments(s)
        .into_iter()
        .map(|seg| match seg {
            emoji::Segment::Text(t) => t.to_string().into_any(),
            emoji::Segment::Emoji(e) => view! { <span class="emoji">{e.to_string()}</span> }.into_any(),
        })
        .collect_view()
}

#[component]
fn Composer() -> impl IntoView {
    let chat = expect_context::<Chat>();
    let file_input: NodeRef<html::Input> = NodeRef::new();
    let dragging = RwSignal::new(false);

    // Drag & drop anywhere on the page while the chat is open. The listeners
    // are removed when you switch to another tab. (No-ops on the server.)
    let depth = StoredValue::new(0i32);
    let handles = vec![
        window_event_listener(leptos::ev::dragenter, move |e| {
            e.prevent_default();
            depth.update_value(|d| *d += 1);
            dragging.set(true);
        }),
        window_event_listener(leptos::ev::dragleave, move |_| {
            depth.update_value(|d| *d -= 1);
            if depth.get_value() <= 0 {
                depth.set_value(0);
                dragging.set(false);
            }
        }),
        window_event_listener(leptos::ev::dragover, |e| e.prevent_default()),
        window_event_listener(leptos::ev::drop, move |e| {
            e.prevent_default();
            depth.set_value(0);
            dragging.set(false);
            if chat.pending.get_untracked() {
                return;
            }
            if let Some(f) = e.data_transfer().and_then(|dt| dt.files()).and_then(|l| l.get(0)) {
                chat.attach(f);
            }
        }),
    ];
    on_cleanup(move || handles.into_iter().for_each(|h| h.remove()));

    view! {
        <div class="composer-wrap">
            <div class="composer" class:dragging=move || dragging.get()>
                <Show when=move || chat.file.with(Option::is_some)>
                    <div class="file-chip">
                        <span class="emoji">"📄"</span>
                        <span class="name">{move || chat.file.with(|f| f.as_ref().map(|f| f.name.clone()))}</span>
                        <span class="size">{move || chat.file.with(|f| f.as_ref().map(|f| format_size(f.size)))}</span>
                        <button type="button" title="Remove file" aria-label="Remove file" on:click=move |_| chat.file.set(None)>
                            "×"
                        </button>
                    </div>
                </Show>
                <div class="row">
                    <button
                        type="button"
                        class="icon-btn"
                        title="Attach a text file (max 2 MB)"
                        aria-label="Attach file"
                        disabled=move || chat.pending.get()
                        on:click=move |_| {
                            if let Some(input) = file_input.get() {
                                input.click();
                            }
                        }
                    >
                        <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                            <path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" />
                        </svg>
                    </button>
                    <Show when=move || chat.can_think.get()>
                        <button
                            type="button"
                            class="think-toggle"
                            class:on=move || chat.think.get()
                            aria-pressed=move || chat.think.get().to_string()
                            disabled=move || chat.pending.get()
                            title="Think: the model reasons step by step before answering. Much more accurate on hard questions, but can take a few minutes."
                            on:click=move |_| chat.think.update(|t| *t = !*t)
                        >
                            <span class="think-bulb" aria-hidden="true"></span>
                            "Think"
                        </button>
                    </Show>
                    <input
                        type="file"
                        hidden
                        node_ref=file_input
                        on:change=move |ev| {
                            let input = event_target::<web_sys::HtmlInputElement>(&ev);
                            if let Some(f) = input.files().and_then(|l| l.get(0)) {
                                chat.attach(f);
                            }
                            input.set_value("");
                        }
                    />
                    <textarea
                        rows="1"
                        placeholder="Message the model…"
                        autofocus
                        node_ref=chat.textarea
                        prop:value=move || chat.input.get()
                        on:input=move |ev| {
                            chat.input.set(event_target_value(&ev));
                            autosize(chat.textarea);
                        }
                        on:keydown=move |ev| {
                            if ev.key() == "Enter" && !ev.shift_key() && !ev.is_composing() {
                                ev.prevent_default();
                                chat.send();
                            }
                        }
                    ></textarea>
                    <button
                        type="button"
                        class="send-orb"
                        title="Send"
                        aria-label="Send"
                        disabled=move || !chat.can_send()
                        on:click=move |_| chat.send()
                    >
                        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round">
                            <path d="M5 12h13M12 5l7 7-7 7" />
                        </svg>
                    </button>
                </div>
            </div>
            <div class="hint">
                <kbd>"Enter"</kbd> " to send · " <kbd>"Shift"</kbd> "+" <kbd>"Enter"</kbd>
                " for a new line · drop a file to attach"
            </div>
        </div>
    }
}

// ======================================================================
// Browser helpers (only ever called from event handlers / spawned tasks)
// ======================================================================

/// Copy buttons inside rendered code blocks (their HTML comes from the
/// markdown renderer, so they're handled by delegation here).
fn on_thread_click(ev: leptos::ev::MouseEvent) {
    let Some(target) = ev.target().and_then(|t| t.dyn_into::<web_sys::Element>().ok()) else {
        return;
    };
    let Ok(Some(button)) = target.closest("[data-copy-code]") else {
        return;
    };
    let code = button
        .closest(".codeblock")
        .ok()
        .flatten()
        .and_then(|block| block.query_selector("code").ok().flatten())
        .and_then(|code| code.text_content())
        .unwrap_or_default();
    copy_text(code, button);
}

fn copy_text(text: String, button: web_sys::Element) {
    let promise = window().navigator().clipboard().write_text(&text);
    spawn_local(async move {
        if wasm_bindgen_futures::JsFuture::from(promise).await.is_ok() {
            let label = button.text_content().unwrap_or_default();
            button.set_text_content(Some("Copied"));
            set_timeout(move || button.set_text_content(Some(&label)), Duration::from_millis(1200));
        }
    });
}

async fn read_text_file(file: web_sys::File) -> Result<PendingFile, String> {
    let name = file.name();
    let size = file.size() as usize;
    if size > MAX_UPLOAD_BYTES {
        return Err(format!("“{name}” is {}. Files must be 2 MB or smaller.", format_size(size)));
    }
    let buffer = wasm_bindgen_futures::JsFuture::from(file.array_buffer())
        .await
        .map_err(|_| format!("Could not read “{name}”."))?;
    let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
    let content = String::from_utf8(bytes)
        .map_err(|_| format!("“{name}” isn't a text file. Only UTF-8 text files can be sent to the model."))?;
    Ok(PendingFile { name, size, content })
}

fn autosize(textarea: NodeRef<html::Textarea>) {
    request_animation_frame(move || {
        if let Some(el) = textarea.get_untracked() {
            let _ = el.set_attribute("style", "height:auto");
            let height = el.scroll_height().min(200);
            let _ = el.set_attribute("style", &format!("height:{height}px"));
        }
    });
}

fn focus(textarea: NodeRef<html::Textarea>) {
    if let Some(el) = textarea.get_untracked() {
        let _ = el.focus();
    }
}

/// Keeps the newest message in view. Without `force`, only scrolls if the
/// reader is already near the bottom, so scrolling up to read isn't hijacked.
fn scroll_to_bottom(scroller: NodeRef<html::Main>, force: bool) {
    let Some(el) = scroller.get_untracked() else { return };
    let near_bottom = el.scroll_height() - el.scroll_top() - el.client_height() < 150;
    if force || near_bottom {
        request_animation_frame(move || el.set_scroll_top(el.scroll_height()));
    }
}

fn format_size(bytes: usize) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}
