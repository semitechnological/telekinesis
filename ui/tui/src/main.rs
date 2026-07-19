use std::io::{stdout, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

use crepuscularity_tui::ratatui::backend::CrosstermBackend;
use crepuscularity_tui::{Template, TemplateContext, TemplateValue};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use rx4::agent::{Agent, Event as Rx4Event, ToolDefinition, ToolEffect, ToolResult};
use rx4::mode::Scope;
use rx4::permissions::ApprovalRequest;
use rx4::provider::{Message, ProviderError, Role, StreamEvent};
use rx4::{register_builtin_tools, ToolRegistry};

mod mcp_config;
#[cfg(feature = "pi-compat")]
mod pi;

const SPINNER_FRAMES: [&str; 10] = [
    "\u{280B}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283C}", "\u{2834}", "\u{2826}", "\u{2827}",
    "\u{2807}", "\u{280F}",
];

const MAX_HISTORY: usize = 100;

fn estimate_tokens(text: &str) -> usize {
    text.chars().count() / 3
}

fn context_color(pct: usize) -> &'static str {
    if pct >= 90 {
        "red-400"
    } else if pct >= 70 {
        "amber-400"
    } else {
        "green-400"
    }
}

fn format_tokens(count: usize) -> String {
    if count < 1000 {
        count.to_string()
    } else if count < 10000 {
        format!("{:.1}k", count as f64 / 1000.0)
    } else if count < 1000000 {
        format!("{}k", count / 1000)
    } else {
        format!("{:.1}M", count as f64 / 1000000.0)
    }
}

fn format_cwd() -> String {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    if let Ok(rel) = cwd.strip_prefix(&home) {
        if rel.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~/{}", rel.display())
        }
    } else {
        cwd.display().to_string()
    }
}

fn history_path() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".telekinesis/input_history.json"))
        .unwrap_or_else(|| PathBuf::from(".telekinesis/input_history.json"))
}

fn load_history() -> Vec<String> {
    std::fs::read_to_string(history_path())
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_default()
}

fn save_history(history: &[String]) {
    let path = history_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let trimmed: Vec<&String> = history.iter().take(MAX_HISTORY).collect();
    let _ = std::fs::write(path, serde_json::to_string(&trimmed).unwrap_or_default());
}

fn spinner_frame(start: Instant) -> String {
    let elapsed = start.elapsed().as_millis();
    let idx = ((elapsed / 100) % SPINNER_FRAMES.len() as u128) as usize;
    SPINNER_FRAMES[idx].to_string()
}

fn blink_cursor(start: Instant) -> String {
    if (start.elapsed().as_millis() / 500).is_multiple_of(2) {
        "▏"
    } else {
        " "
    }
    .to_string()
}

fn template_path() -> Option<PathBuf> {
    let candidates = [
        dirs::home_dir().map(|h| h.join(".telekinesis/shell.crepus")),
        dirs::home_dir().map(|h| h.join(".local/share/telekinesis/shell.crepus")),
        Some(PathBuf::from("ui/shell.crepus")),
        Some(PathBuf::from("shell.crepus")),
    ];
    candidates.into_iter().flatten().find(|p| p.exists())
}

struct OpenAICompatProvider {
    id: String,
    base_url: String,
    api_key: String,
    client: reqwest::Client,
}

impl OpenAICompatProvider {
    fn new(id: &str, base_url: &str, api_key: &str) -> Self {
        Self {
            id: id.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            client: reqwest::Client::new(),
        }
    }

    fn from_env() -> Option<Self> {
        if let Ok(key) = std::env::var("XAI_API_KEY") {
            return Some(Self::new("xai", "https://api.x.ai/v1", &key));
        }
        if let Ok(key) = std::env::var("OPENAI_API_KEY") {
            return Some(Self::new("openai", "https://api.openai.com/v1", &key));
        }
        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
            return Some(Self::new("anthropic", "https://api.anthropic.com/v1", &key));
        }
        if let Ok(key) = std::env::var("GOOGLE_API_KEY") {
            return Some(Self::new(
                "google",
                "https://generativelanguage.googleapis.com/v1beta",
                &key,
            ));
        }
        None
    }

    fn default_model(&self) -> &str {
        match self.id.as_str() {
            "xai" => "grok-4.5",
            "openai" => "gpt-4o",
            "anthropic" => "claude-3-5-sonnet-20241022",
            "google" => "gemini-2.0-flash",
            _ => "gpt-4o",
        }
    }
}

impl rx4::provider::Provider for OpenAICompatProvider {
    fn id(&self) -> &str {
        &self.id
    }
    fn name(&self) -> &str {
        "OpenAI Compatible"
    }

    fn stream<
        'life0: 'async_trait,
        'life1: 'async_trait,
        'life2: 'async_trait,
        'life3: 'async_trait,
        'life4: 'async_trait,
        'async_trait,
    >(
        &'life0 self,
        messages: &'life1 [Message],
        system: &'life2 Option<String>,
        model: &'life3 str,
        _tools: &'life4 [serde_json::Value],
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        Box<
                            dyn futures::Stream<Item = Result<StreamEvent, ProviderError>>
                                + Send
                                + Unpin
                                + 'static,
                        >,
                        ProviderError,
                    >,
                > + Send
                + 'async_trait,
        >,
    >
    where
        Self: 'async_trait,
    {
        let base_url = self.base_url.clone();
        let api_key = self.api_key.clone();
        let provider_id = self.id.clone();
        let model = model.to_string();
        let messages = messages.to_vec();
        let system = system.clone();
        let client = self.client.clone();

        Box::pin(async move {
            let mut req_messages: Vec<serde_json::Value> = Vec::new();
            if let Some(s) = &system {
                req_messages.push(serde_json::json!({"role": "system", "content": s}));
            }
            for msg in &messages {
                let role = match msg.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::System => "system",
                    Role::Tool => "tool",
                };
                req_messages.push(serde_json::json!({"role": role, "content": msg.content}));
            }

            // Anthropic prompt-cache markers when talking to Anthropic-compatible endpoints.
            if provider_id == "anthropic" {
                rx4::apply_cache_control(
                    &mut req_messages,
                    &rx4::PromptCacheConfig::anthropic(),
                );
            }

            let body = serde_json::json!({
                "model": model,
                "messages": req_messages,
                "stream": true,
            });

            let url = format!("{}/chat/completions", base_url);
            let resp = client
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| ProviderError::Http(e.to_string()))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(ProviderError::Api(format!("{}: {}", status, text)));
            }

            let stream = resp
                .bytes_stream()
                .map(|chunk| chunk.map_err(std::io::Error::other));

            let event_stream = SseStream::new(stream);
            let rx4_stream = event_stream.filter_map(|event| async move {
                match event {
                    Ok(data) => {
                        if data.is_empty() || data == "[DONE]" {
                            return Some(Ok(StreamEvent::Done));
                        }
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) {
                            if let Some(delta) = json
                                .get("choices")
                                .and_then(|c| c.get(0))
                                .and_then(|c| c.get("delta"))
                                .and_then(|d| d.get("content"))
                                .and_then(|c| c.as_str())
                            {
                                if !delta.is_empty() {
                                    return Some(Ok(StreamEvent::Delta(delta.to_string())));
                                }
                            }
                            if let Some(finish) = json
                                .get("choices")
                                .and_then(|c| c.get(0))
                                .and_then(|c| c.get("finish_reason"))
                                .and_then(|f| f.as_str())
                            {
                                if !finish.is_empty() {
                                    return Some(Ok(StreamEvent::Done));
                                }
                            }
                        }
                        None
                    }
                    Err(_) => Some(Err(ProviderError::Stream("stream error".to_string()))),
                }
            });

            Ok(Box::new(Box::pin(rx4_stream))
                as Box<
                    dyn futures::Stream<Item = Result<StreamEvent, ProviderError>> + Send + Unpin,
                >)
        })
    }
}

struct SseStream<S> {
    inner: S,
    buffer: String,
}

impl<S> SseStream<S> {
    fn new(inner: S) -> Self {
        Self {
            inner,
            buffer: String::new(),
        }
    }
}

impl<S, E> futures::Stream for SseStream<S>
where
    S: futures::Stream<Item = Result<bytes::Bytes, E>> + Unpin,
{
    type Item = Result<String, E>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(pos) = this.buffer.find("\n\n") {
                let block: String = this.buffer.drain(..pos + 2).collect();
                for line in block.lines() {
                    if let Some(data) = line.strip_prefix("data: ") {
                        return std::task::Poll::Ready(Some(Ok(data.to_string())));
                    }
                }
                continue;
            }
            match futures::Stream::poll_next(std::pin::Pin::new(&mut this.inner), cx) {
                std::task::Poll::Ready(Some(Ok(chunk))) => {
                    this.buffer.push_str(&String::from_utf8_lossy(&chunk));
                    continue;
                }
                std::task::Poll::Ready(Some(Err(e))) => {
                    return std::task::Poll::Ready(Some(Err(e)));
                }
                std::task::Poll::Ready(None) => {
                    return std::task::Poll::Ready(None);
                }
                std::task::Poll::Pending => {
                    return std::task::Poll::Pending;
                }
            }
        }
    }
}

#[derive(Clone)]
struct ChatMessage {
    role: String,
    content: String,
    is_tool: bool,
    tool_name: String,
    is_streaming: bool,
}

struct App {
    input: String,
    messages: Vec<ChatMessage>,
    model: String,
    busy: bool,
    auto_scroll: bool,
    input_history: Vec<String>,
    history_index: Option<usize>,
    history_draft: String,
    input_tokens: usize,
    output_tokens: usize,
    cost: f64,
    spinner_start: Instant,
    cursor_start: Instant,
    show_header: bool,
    permission_prompt: bool,
    permission_tool: String,
    session_name: String,
    context_pct: usize,
    context_window: usize,
    agent: Option<Arc<Mutex<Agent>>>,
    event_rx: Option<tokio::sync::mpsc::UnboundedReceiver<AppEvent>>,
    prompt_char: String,
    /// Fully-qualified MCP tool names registered at startup (`mcp__server__tool`).
    mcp_tools: Vec<String>,
}

enum AppEvent {
    Rx4(Rx4Event),
    Error(String),
    Idle,
}

impl App {
    fn new() -> Self {
        Self {
            input: String::new(),
            messages: Vec::new(),
            model: "no-model".to_string(),
            busy: false,
            auto_scroll: true,
            input_history: load_history(),
            history_index: None,
            history_draft: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            cost: 0.0,
            spinner_start: Instant::now(),
            cursor_start: Instant::now(),
            show_header: true,
            permission_prompt: false,
            permission_tool: String::new(),
            session_name: "default".to_string(),
            context_pct: 0,
            context_window: 128_000,
            agent: None,
            event_rx: None,
            prompt_char: ">".to_string(),
            mcp_tools: Vec::new(),
        }
    }

    fn update_template(&self, tpl: &mut Template) {
        tpl.set("input", self.input.clone());
        tpl.set("input_len", self.input.chars().count() as i64);
        tpl.set("model", self.model.clone());
        tpl.set("busy", self.busy);
        tpl.set("auto_scroll", self.auto_scroll);
        tpl.set("version", "0.2.0");
        tpl.set("session_name", self.session_name.clone());
        tpl.set("show_header", self.show_header);
        tpl.set("spinner", spinner_frame(self.spinner_start));
        tpl.set("cursor", blink_cursor(self.cursor_start));
        tpl.set("prompt_char", self.prompt_char.clone());
        tpl.set("permission_prompt", self.permission_prompt);
        tpl.set("permission_tool", self.permission_tool.clone());
        tpl.set("pwd", format_cwd());
        tpl.set("input_tokens", format_tokens(self.input_tokens));
        tpl.set("output_tokens", format_tokens(self.output_tokens));
        tpl.set("cost", format!("{:.3}", self.cost));
        tpl.set("context_pct", self.context_pct.to_string());
        tpl.set("context_window", format_tokens(self.context_window));
        tpl.set("context_color", context_color(self.context_pct));

        let msgs: Vec<TemplateContext> = self
            .messages
            .iter()
            .map(|m| {
                let mut mc = TemplateContext::new();
                mc.set("is_user", m.role == "user");
                mc.set("is_tool", m.is_tool);
                mc.set("tool_name", m.tool_name.clone());
                mc.set("is_streaming", m.is_streaming);
                let lines: Vec<TemplateContext> = m
                    .content
                    .lines()
                    .map(|line| {
                        let mut lc = TemplateContext::new();
                        lc.set("text", line.to_string());
                        lc
                    })
                    .collect();
                mc.set("lines", TemplateValue::List(lines));
                mc
            })
            .collect();
        tpl.set("messages", TemplateValue::List(msgs));
    }

    fn submit_prompt(
        &mut self,
        agent: &Arc<Mutex<Agent>>,
        tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
    ) {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return;
        }

        self.input_history.insert(0, text.clone());
        save_history(&self.input_history);
        self.history_index = None;

        self.messages.push(ChatMessage {
            role: "user".to_string(),
            content: text.clone(),
            is_tool: false,
            tool_name: String::new(),
            is_streaming: false,
        });

        self.input.clear();
        self.busy = true;

        let agent = agent.clone();
        tokio::spawn(async move {
            let mut agent = agent.lock().await;
            let result = agent.prompt(&text).await;
            if let Err(e) = result {
                let _ = tx.send(AppEvent::Error(e.to_string()));
            }
            let _ = tx.send(AppEvent::Idle);
        });
    }

    fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Rx4(e) => self.handle_rx4_event(e),
            AppEvent::Error(msg) => {
                self.messages.push(ChatMessage {
                    role: "error".to_string(),
                    content: format!("Error: {msg}"),
                    is_tool: false,
                    tool_name: String::new(),
                    is_streaming: false,
                });
            }
            AppEvent::Idle => {
                self.busy = false;
            }
        }
    }

    fn handle_rx4_event(&mut self, event: Rx4Event) {
        if let Rx4Event::ApprovalRequired(req) = &event {
            self.permission_prompt = true;
            self.permission_tool = format_approval(req);
        }
        match event {
            Rx4Event::AgentStart => {}
            Rx4Event::TurnStart { .. } => {
                self.messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: String::new(),
                    is_tool: false,
                    tool_name: String::new(),
                    is_streaming: true,
                });
            }
            Rx4Event::MessageStart { role } => {
                if role == Role::Assistant
                    && self
                        .messages
                        .last()
                        .is_none_or(|m| m.role != "assistant" || !m.content.is_empty())
                {
                    self.messages.push(ChatMessage {
                        role: "assistant".to_string(),
                        content: String::new(),
                        is_tool: false,
                        tool_name: String::new(),
                        is_streaming: true,
                    });
                }
            }
            Rx4Event::MessageDelta { delta } => {
                if let Some(msg) = self.messages.last_mut() {
                    msg.content.push_str(&delta);
                }
            }
            Rx4Event::MessageEnd { content, .. } => {
                let tokens = estimate_tokens(&content);
                if let Some(msg) = self.messages.last_mut() {
                    if !content.is_empty() {
                        msg.content = content;
                    }
                    msg.is_streaming = false;
                }
                self.output_tokens += tokens;
            }
            Rx4Event::ToolCall(call) => {
                self.messages.push(ChatMessage {
                    role: "tool".to_string(),
                    content: truncate_args(&call.arguments, 240),
                    is_tool: true,
                    tool_name: call.name,
                    is_streaming: false,
                });
            }
            Rx4Event::ApprovalRequired(req) => {
                self.permission_prompt = true;
                self.permission_tool = format_approval(&req);
                self.messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: format!(
                        "Approval required: {} ({})\nargs: {}",
                        req.tool_name,
                        req.reason,
                        truncate_args(&req.arguments, 400)
                    ),
                    is_tool: false,
                    tool_name: String::new(),
                    is_streaming: false,
                });
            }
            Rx4Event::ToolExecutionStart(_) => {}
            Rx4Event::ToolExecutionEnd(result) => {
                if let Some(msg) = self.messages.last_mut() {
                    if msg.is_tool {
                        msg.content = result.content;
                    }
                }
            }
            Rx4Event::TurnEnd { .. } => {}
            Rx4Event::AgentEnd => {
                if let Some(msg) = self.messages.last_mut() {
                    msg.is_streaming = false;
                }
            }
            Rx4Event::Error(msg) => {
                self.messages.push(ChatMessage {
                    role: "error".to_string(),
                    content: format!("Error: {msg}"),
                    is_tool: false,
                    tool_name: String::new(),
                    is_streaming: false,
                });
            }
        }
    }

    fn history_get(&self) -> String {
        if let Some(idx) = self.history_index {
            self.input_history.get(idx).cloned().unwrap_or_default()
        } else {
            String::new()
        }
    }
}

fn run_login(provider: Option<&str>) -> anyhow::Result<()> {
    let provider = provider.unwrap_or("grok");
    let oauth_provider = match provider {
        "grok" | "xai" => rs_ai_oauth::OAuthProvider::Xai,
        "openai" | "chatgpt" => rs_ai_oauth::OAuthProvider::ChatGpt,
        "claude" | "anthropic" => rs_ai_oauth::OAuthProvider::Claude,
        "gemini" | "google" => rs_ai_oauth::OAuthProvider::Gemini,
        "copilot" => rs_ai_oauth::OAuthProvider::Copilot,
        "kimi" => rs_ai_oauth::OAuthProvider::Kimi,
        "antigravity" => rs_ai_oauth::OAuthProvider::Antigravity,
        _ => {
            eprintln!("Unknown provider: {provider}");
            eprintln!("Available: grok, openai, claude, gemini, copilot, kimi, antigravity");
            std::process::exit(1);
        }
    };
    println!("Starting OAuth flow for {provider}...");
    let tokens = rs_ai_oauth::start_oauth_flow(oauth_provider)?;
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let dir = home.join(".telekinesis");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{provider}_token.json"));
    std::fs::write(&path, serde_json::to_string_pretty(&tokens)?)?;
    println!("Token saved to {}", path.display());
    Ok(())
}

fn run_tui() -> anyhow::Result<()> {
    let tpl_path = template_path().ok_or_else(|| {
        anyhow::anyhow!("shell.crepus template not found. Checked ~/.telekinesis/shell.crepus and ui/shell.crepus")
    })?;
    let mut tpl = Template::from_path(&tpl_path).map_err(|e| anyhow::anyhow!("{e}"))?;

    let provider = OpenAICompatProvider::from_env().ok_or_else(|| {
        anyhow::anyhow!(
            "No API key found. Set one of: XAI_API_KEY, OPENAI_API_KEY, ANTHROPIC_API_KEY, GOOGLE_API_KEY\n\
             Or run `tk login grok` for OAuth."
        )
    })?;

    let model = provider.default_model().to_string();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();

    let mut agent = Agent::new();
    agent.set_scope(Scope::Coding);
    let mut tools = ToolRegistry::new();
    register_builtin_tools(&mut tools);
    rx4::computer_use::register_tools(&mut tools);
    let mcp_tools = rt.block_on(connect_mcp_tools(&mut tools));
    agent.set_tools(tools);
    agent.set_workspace_root(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    agent.load_project_context();
    agent.set_model(&model);
    agent.set_provider(Arc::new(provider));
    let workspace = agent.workspace_root.clone();
    agent.set_sandbox(Arc::new(rx4::SandboxManager::new(
        rx4::SandboxProfile::Workspace,
        workspace,
    )));
    // Policy.workspace_write enables OS sandbox flag; enable_os_sandbox installs runner.
    agent.set_policy(rx4::Policy::workspace_write().with_os_sandbox(true));
    let _ = agent.enable_os_sandbox();
    if let Some(home) = dirs::home_dir() {
        let mut engine = rx4::SkillEngine::new(home.join(".agents").join("skills"));
        if engine.load().is_ok() {
            let mut reg = rx4::SkillRegistry::new();
            for skill in engine.list() {
                reg.register(skill.clone());
            }
            agent.set_skill_registry(reg);
            agent.set_skill_engine(engine);
        }
    }
    agent.set_graph_memory(rx4::GraphMemory::new());
    agent.enable_auto_dream(true);

    let event_tx_clone = event_tx.clone();
    agent.subscribe(move |event: &Rx4Event| {
        let _ = event_tx_clone.send(AppEvent::Rx4(event.clone()));
    });

    let agent = Arc::new(Mutex::new(agent));

    let mut app = App::new();
    app.model = model;
    app.agent = Some(agent.clone());
    app.event_rx = Some(event_rx);
    app.mcp_tools = mcp_tools;

    let _rt_guard = rt.enter();

    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    stdout.flush()?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = crepuscularity_tui::ratatui::Terminal::new(backend)?;

    loop {
        app.update_template(&mut tpl);
        if !tpl.changed_keys().is_empty() {
            terminal.draw(|f| {
                if let Err(e) = tpl.draw(f, f.area()) {
                    use crepuscularity_tui::ratatui::style::Style;
                    use crepuscularity_tui::ratatui::widgets::Paragraph;
                    let p = Paragraph::new(format!("Template error: {e}"))
                        .style(Style::default().fg(crepuscularity_tui::ratatui::style::Color::Red));
                    f.render_widget(p, f.area());
                }
            })?;
            tpl.mark_rendered();
        }

        let mut pending = Vec::new();
        if let Some(rx) = app.event_rx.as_mut() {
            while let Ok(event) = rx.try_recv() {
                pending.push(event);
            }
        }
        for event in pending {
            app.handle_event(event);
        }

        if crossterm::event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = crossterm::event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match (key.code, key.modifiers) {
                    (KeyCode::Enter, _) => {
                        if app.busy {
                            continue;
                        }
                        let text = app.input.trim().to_string();
                        if text == "/quit" || text == "/exit" {
                            break;
                        }
                        if text.starts_with('/') {
                            handle_slash_command(&mut app, &text, &agent, &event_tx);
                        } else if !text.is_empty() {
                            app.submit_prompt(&agent, event_tx.clone());
                        }
                    }
                    (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        if app.busy {
                            app.busy = false;
                        } else {
                            break;
                        }
                    }
                    (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                        if app.input.is_empty() {
                            break;
                        }
                    }
                    (KeyCode::Char('l'), KeyModifiers::CONTROL) => {
                        let _ = terminal.clear();
                    }
                    (KeyCode::Char('b'), KeyModifiers::CONTROL) => {
                        app.show_header = !app.show_header;
                    }
                    (KeyCode::Up, _) => {
                        if app.history_index.is_none() && !app.input_history.is_empty() {
                            app.history_draft = app.input.clone();
                            app.history_index = Some(0);
                            app.input = app.history_get();
                        } else if let Some(idx) = app.history_index {
                            if idx + 1 < app.input_history.len() {
                                app.history_index = Some(idx + 1);
                                app.input = app.history_get();
                            }
                        }
                    }
                    (KeyCode::Down, _) => {
                        if let Some(idx) = app.history_index {
                            if idx == 0 {
                                app.history_index = None;
                                app.input = app.history_draft.clone();
                            } else {
                                app.history_index = Some(idx - 1);
                                app.input = app.history_get();
                            }
                        }
                    }
                    (KeyCode::Backspace, _) => {
                        app.input.pop();
                    }
                    (KeyCode::PageUp, _) => {
                        app.auto_scroll = false;
                    }
                    (KeyCode::PageDown, _) => {
                        app.auto_scroll = true;
                    }
                    (KeyCode::Home, _) => {
                        app.auto_scroll = false;
                    }
                    (KeyCode::End, _) => {
                        app.auto_scroll = true;
                    }
                    (KeyCode::Char(c), _) => {
                        app.input.push(c);
                    }
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.backend_mut().flush()?;
    Ok(())
}

fn truncate_args(args: &str, max: usize) -> String {
    let flat = args.replace('\n', " ");
    if flat.chars().count() <= max {
        flat
    } else {
        let mut out: String = flat.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn format_approval(req: &ApprovalRequest) -> String {
    format!(
        "{} — {} | args: {}",
        req.tool_name,
        req.reason,
        truncate_args(&req.arguments, 200)
    )
}

/// Best-effort MCP connect from ~/.telekinesis/mcp.json. Never fails TUI startup.
async fn connect_mcp_tools(tools: &mut ToolRegistry) -> Vec<String> {
    let configs = mcp_config::load();
    if configs.is_empty() {
        return Vec::new();
    }

    let mut names = Vec::new();
    for cfg in configs {
        let transport = cfg.transport.to_ascii_lowercase();
        let client = match transport.as_str() {
            "http" => {
                let Some(url) = cfg.url.as_deref() else {
                    eprintln!(
                        "telekinesis: MCP server `{}` missing url for http transport",
                        cfg.name
                    );
                    continue;
                };
                let headers = if cfg.headers.is_empty() {
                    None
                } else {
                    Some(cfg.headers.clone())
                };
                rx4::McpClient::connect_http(url, headers).await
            }
            "sse" => {
                let Some(url) = cfg.url.as_deref() else {
                    eprintln!(
                        "telekinesis: MCP server `{}` missing url for sse transport",
                        cfg.name
                    );
                    continue;
                };
                let headers = if cfg.headers.is_empty() {
                    None
                } else {
                    Some(cfg.headers.clone())
                };
                rx4::McpClient::connect_sse(url, headers).await
            }
            _ => {
                let Some(command) = cfg.command.as_deref() else {
                    eprintln!(
                        "telekinesis: MCP server `{}` missing command for stdio transport",
                        cfg.name
                    );
                    continue;
                };
                let arg_refs: Vec<&str> = cfg.args.iter().map(String::as_str).collect();
                rx4::McpClient::connect_stdio(command, &arg_refs).await
            }
        };

        match client {
            Ok(client) => match client.list_tools().await {
                Ok(listed) => {
                    let client = Arc::new(client);
                    for tool in listed {
                        let full = format!("mcp__{}__{}", cfg.name, tool.name);
                        let desc = if tool.description.is_empty() {
                            format!("MCP tool {} from {}", tool.name, cfg.name)
                        } else {
                            tool.description.clone()
                        };
                        let params = tool.input_schema.to_string();
                        let client_c = client.clone();
                        let remote_name = tool.name.clone();
                        tools.register(
                            ToolDefinition::new_boxed(
                                full.clone(),
                                desc,
                                params,
                                Box::new(move |_ctx, args| {
                                    let client = client_c.clone();
                                    let remote_name = remote_name.clone();
                                    Box::pin(async move {
                                        let value: serde_json::Value =
                                            serde_json::from_str(&args).unwrap_or_else(|_| {
                                                serde_json::json!({ "raw": args })
                                            });
                                        match client.call_tool(&remote_name, &value).await {
                                            Ok(v) => ToolResult::ok(
                                                remote_name.clone(),
                                                v.to_string(),
                                            ),
                                            Err(e) => ToolResult::err(
                                                remote_name.clone(),
                                                e.to_string(),
                                            ),
                                        }
                                    })
                                }),
                            )
                            .with_effect(ToolEffect::Network),
                        );
                        names.push(full);
                    }
                }
                Err(e) => {
                    eprintln!(
                        "telekinesis: MCP list_tools failed for `{}`: {e}",
                        cfg.name
                    );
                }
            },
            Err(e) => {
                eprintln!("telekinesis: MCP connect failed for `{}`: {e}", cfg.name);
            }
        }
    }
    names
}


fn handle_slash_command(
    app: &mut App,
    cmd: &str,
    _agent: &Arc<Mutex<Agent>>,
    _tx: &tokio::sync::mpsc::UnboundedSender<AppEvent>,
) {
    let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
    let command = parts[0];
    let arg = parts.get(1).copied().unwrap_or("");

    match command {
        "/quit" | "/exit" => {}
        "/clear" => {
            app.messages.clear();
            app.input_tokens = 0;
            app.output_tokens = 0;
            app.cost = 0.0;
        }
        "/help" => {
            app.messages.push(ChatMessage {
                role: "system".to_string(),
                content: "Commands: /model <name>, /scope <coding|research|plan|ask|computer_use>, /mcp, /todo, /clear, /cost, /help, /quit\nKeys: Ctrl+B toggle header, Ctrl+L clear screen, Ctrl+C interrupt, Up/Down history, PgUp/PgDn scroll chat".to_string(),
                is_tool: false,
                tool_name: String::new(),
                is_streaming: false,
            });
        }
        "/model" => {
            if arg.is_empty() {
                app.messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: format!("Current model: {}", app.model),
                    is_tool: false,
                    tool_name: String::new(),
                    is_streaming: false,
                });
            } else {
                app.model = arg.to_string();
                if let Some(a) = &app.agent {
                    if let Ok(mut agent) = a.try_lock() {
                        agent.set_model(arg);
                    }
                }
                app.messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: format!("Model set to: {arg}"),
                    is_tool: false,
                    tool_name: String::new(),
                    is_streaming: false,
                });
            }
        }
        "/cost" => {
            app.messages.push(ChatMessage {
                role: "system".to_string(),
                content: format!(
                    "Input: {} tokens, Output: {} tokens, Cost: ${:.4}",
                    app.input_tokens, app.output_tokens, app.cost
                ),
                is_tool: false,
                tool_name: String::new(),
                is_streaming: false,
            });
        }
        "/scope" => {
            if let Some(a) = &app.agent {
                if let Ok(mut agent) = a.try_lock() {
                    let scope = match arg {
                        "coding" => Scope::Coding,
                        "research" => Scope::Research,
                        "plan" => Scope::Plan,
                        "ask" => Scope::Ask,
                        "computer_use" | "computer-use" | "cu" => Scope::ComputerUse,
                        _ => Scope::Coding,
                    };
                    agent.set_scope(scope);
                }
            }
            app.messages.push(ChatMessage {
                role: "system".to_string(),
                content: format!("Scope set to: {arg}"),
                is_tool: false,
                tool_name: String::new(),
                is_streaming: false,
            });
        }
        "/mcp" => {
            let path = mcp_config::config_path();
            let body = if app.mcp_tools.is_empty() {
                format!(
                    "No MCP tools connected.\nConfig: {}\nFormat: {{\"servers\":[{{\"name\":\"fs\",\"transport\":\"stdio\",\"command\":\"npx\",\"args\":[\"-y\",\"@modelcontextprotocol/server-filesystem\",\".\"]}}]}}\nRemote HTTP/SSE: put url+transport=http|sse in config (host loader documents it; engine stdio works today).",
                    path.display()
                )
            } else {
                format!(
                    "MCP tools ({}):\n{}\nConfig: {}",
                    app.mcp_tools.len(),
                    app.mcp_tools.join("\n"),
                    path.display()
                )
            };
            app.messages.push(ChatMessage {
                role: "system".to_string(),
                content: body,
                is_tool: false,
                tool_name: String::new(),
                is_streaming: false,
            });
        }
        "/todo" => {
            app.messages.push(ChatMessage {
                role: "system".to_string(),
                content: "/todo: host surface only. Engine may expose todo tool later — track work in chat or project TODO for now.".to_string(),
                is_tool: false,
                tool_name: String::new(),
                is_streaming: false,
            });
        }
        _ => {
            app.messages.push(ChatMessage {
                role: "system".to_string(),
                content: format!("Unknown command: {command}. Type /help for available commands."),
                is_tool: false,
                tool_name: String::new(),
                is_streaming: false,
            });
        }
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 2 && args[1] == "login" {
        return run_login(args.get(2).map(|s| s.as_str()));
    }
    if args.len() >= 2 && (args[1] == "--help" || args[1] == "-h") {
        println!("telekinesis (tk) — AI coding agent TUI");
        println!();
        println!("USAGE:");
        println!("  tk              Start interactive TUI");
        println!(
            "  tk login <provider>  OAuth login (grok, openai, claude, gemini, copilot, kimi)"
        );
        println!("  tk --help       Show this help");
        println!();
        println!("ENVIRONMENT:");
        println!("  XAI_API_KEY         xAI Grok API key");
        println!("  OPENAI_API_KEY      OpenAI API key");
        println!("  ANTHROPIC_API_KEY   Anthropic Claude API key");
        println!("  GOOGLE_API_KEY      Google Gemini API key");
        println!();
        println!("KEYS:");
        println!("  Enter        Submit prompt");
        println!("  Ctrl+C       Interrupt / exit");
        println!("  Ctrl+L       Clear screen");
        println!("  Ctrl+B       Toggle header");
        println!("  Up/Down      Input history");
        println!("  PgUp/PgDn    Scroll chat view");
        println!("  Home/End     Jump to top/bottom of chat");
        return Ok(());
    }

    run_tui()
}
