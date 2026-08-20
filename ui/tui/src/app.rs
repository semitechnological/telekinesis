use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crepuscularity_tui::ratatui::text::Line;
use crepuscularity_tui::{Template, TemplateContext, TemplateValue};
use parking_lot::Mutex as ParkingMutex;
use rx4::agent::{Agent, AgentError, CancellationHandle, Event as Rx4Event, ToolSource};
use rx4::permissions::{Decision, PlanDecision, PlanProposal};
use rx4::provider::{Provider, Role};
use rx4::subagent::{SubagentManager, SubagentStatus};
use rx4::{ModelInfo, ModelRegistry};
use tokio::sync::Mutex;

use crate::channel_approver::{ApprovalMode, PendingApproval};
use crate::host::{self, load_history, save_history, save_prefs, Prefs};
use crate::markdown;
use crate::models::{
    context_window_for_model, host_model_info, oauth_model_info, openrouter_model_info,
    PI_CODEX_GPT56, PI_OPENAI_GPT5,
};
#[cfg(feature = "pi-compat")]
use crate::pi::{PiEntryType, PiSession};
use crate::provider_catalog;
use crate::providers::{
    configured_provider_id, providers_summary, push_system_message, run_login_from_tui,
};
use crate::tui::{
    bounded_plan_preview, tool_color, tool_detail, tool_result_summary, wrap_scrollback_line,
};

pub(crate) const SPINNER_FRAMES: [&str; 10] = [
    "\u{280B}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283C}", "\u{2834}", "\u{2826}", "\u{2827}",
    "\u{2807}", "\u{280F}",
];

pub(crate) const LARGE_PASTE_LINES: usize = 10;
pub(crate) const LARGE_PASTE_CHARS: usize = 1000;
pub(crate) const PLAN_PREVIEW_MAX_LINES: usize = 24;
pub(crate) const PLAN_PREVIEW_LINE_LIMIT: usize = 400;
pub(crate) const MAX_BUDGET_DURATION_SECONDS: u64 = 24 * 60 * 60;
pub(crate) const MAX_BUDGET_TURNS: usize = rx4::guardrails::MAX_TOOL_ITERATIONS_CEILING;
/// Coalesce `git ls-files` searches while the user types an `@` mention.
pub(crate) const FILE_SEARCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(150);
/// Throttle JSONL session appends (fsync per tool event is wasteful).
pub(crate) const SESSION_PERSIST_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(500);

/// (command, description) — pi-style autocomplete shows the description next
/// to each command name.
pub(crate) const SLASH_COMMANDS: [(&str, &str); 23] = [
    ("/login", "sign in with a provider"),
    ("/providers", "browse and configure providers"),
    ("/provider", "alias for /providers"),
    ("/apikey", "show API-key setup for a provider"),
    ("/keys", "alias for /apikey"),
    ("/auth", "alias for /providers"),
    ("/config", "interactive config menu"),
    ("/model", "pick or set the model"),
    (
        "/scope",
        if cfg!(feature = "computer-use") {
            "coding · research · plan · ask · computer_use"
        } else {
            "coding · research · plan · ask"
        },
    ),
    ("/plan", "read-only implementation plan"),
    ("/review", "read-only review of the workspace"),
    ("/subagent", "spawn · list · cancel subagents"),
    ("/budget", "set cost, time, or turn limits"),
    ("/plan-approval", "approve or bypass whole-turn plans"),
    (
        "/mcp",
        if cfg!(feature = "mcp") {
            "list MCP tools + config help"
        } else {
            "requires rebuild --features mcp (or full)"
        },
    ),
    (
        "/search",
        if cfg!(feature = "search") {
            "web_search tool (darash)"
        } else {
            "requires rebuild --features search (or full)"
        },
    ),
    ("/todo", "session todo note"),
    ("/clear", "clear messages and reset cost"),
    ("/cost", "show cost breakdown"),
    (
        "/commands",
        "list commands (with /commands <name> for usage)",
    ),
    ("/help", "list commands and keys"),
    ("/quit", "quit"),
    ("/exit", "quit"),
];

pub(crate) fn slash_description(command: &str) -> &'static str {
    SLASH_COMMANDS
        .iter()
        .find(|(name, _)| *name == command)
        .map(|(_, description)| *description)
        .unwrap_or("")
}

pub(crate) fn context_color(pct: usize) -> &'static str {
    if pct >= 90 {
        "red-400"
    } else if pct >= 70 {
        "amber-400"
    } else {
        "green-400"
    }
}

pub(crate) fn effort_color(effort: &str) -> &'static str {
    match effort {
        "low" => "green-400",
        "medium" => "blue-400",
        "high" => "amber-400",
        _ => "fuchsia-400",
    }
}

pub(crate) fn format_tokens(count: usize) -> String {
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

pub(crate) fn project_name() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "-".to_string())
}

pub(crate) fn git_branch() -> String {
    std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|branch| branch.trim().to_string())
        .filter(|branch| !branch.is_empty())
        .unwrap_or_else(|| "-".to_string())
}

pub(crate) fn spinner_frame(start: Instant) -> &'static str {
    let elapsed = start.elapsed().as_millis();
    let idx = ((elapsed / 100) % SPINNER_FRAMES.len() as u128) as usize;
    SPINNER_FRAMES[idx]
}

pub(crate) fn file_query(input: &str) -> Option<&str> {
    let token = input
        .rsplit_once(char::is_whitespace)
        .map_or(input, |(_, token)| token);
    token.strip_prefix('@')
}

pub(crate) fn matching_slash_commands(input: &str) -> Vec<String> {
    if input.starts_with('/') && !input.contains(char::is_whitespace) {
        SLASH_COMMANDS
            .iter()
            .filter(|(command, _)| command.starts_with(input))
            .map(|(command, _)| (*command).to_string())
            .collect()
    } else {
        Vec::new()
    }
}

/// Port of pi's fuzzy matcher (`packages/tui/src/fuzzy.ts`): subsequence match
/// with word-boundary and consecutive-match bonuses, plus a letter↔digit swap
/// fallback ("gpt55" finds "gpt-5.5"). Lower score = better match; `None` when
/// the query does not match the text at all.
pub(crate) fn fuzzy_match(query: &str, text: &str) -> Option<i64> {
    let query: Vec<char> = query.to_lowercase().chars().collect();
    let text: Vec<char> = text.to_lowercase().chars().collect();
    if query.is_empty() {
        return Some(0);
    }
    if query.len() > text.len() {
        return None;
    }

    fn score_query(query: &[char], text: &[char]) -> Option<i64> {
        let mut query_index = 0usize;
        let mut score = 0i64;
        let mut last_match: Option<usize> = None;
        let mut consecutive = 0i64;
        for (index, &character) in text.iter().enumerate() {
            if query_index >= query.len() {
                break;
            }
            if character == query[query_index] {
                let is_word_boundary =
                    index == 0 || matches!(text[index - 1], ' ' | '-' | '_' | '.' | '/' | ':');
                if last_match.is_some_and(|last| last + 1 == index) {
                    consecutive += 1;
                    score -= consecutive * 5;
                } else {
                    consecutive = 0;
                    if let Some(last) = last_match {
                        score += (index - last - 1) as i64 * 2;
                    }
                }
                if is_word_boundary {
                    score -= 10;
                }
                score += (index as i64) / 10;
                last_match = Some(index);
                query_index += 1;
            }
        }
        if query_index < query.len() {
            return None;
        }
        if query == text {
            score -= 100;
        }
        Some(score)
    }

    let primary = score_query(&query, &text);
    if primary.is_some() {
        return primary;
    }

    // pi's swap fallback: a query that is entirely letters+digits can also be
    // tried with the letter/digit halves swapped ("gpt55" ↔ "55gpt").
    let letters: String = query.iter().filter(|c| c.is_alphabetic()).collect();
    let digits: String = query.iter().filter(|c| c.is_ascii_digit()).collect();
    if !letters.is_empty()
        && !digits.is_empty()
        && query
            .iter()
            .all(|c| c.is_alphabetic() || c.is_ascii_digit())
    {
        let swapped = if query[0].is_alphabetic() {
            format!("{digits}{letters}")
        } else {
            format!("{letters}{digits}")
        };
        if let Some(score) = score_query(&swapped.chars().collect::<Vec<_>>(), &text) {
            return Some(score + 5);
        }
    }
    primary
}

/// Filter and rank `items` by fuzzy match quality (pi's `fuzzyFilter`): whitespace
/// and `/`-separated tokens must all match, best matches first.
pub(crate) fn fuzzy_filter<'a, T>(
    items: &'a [T],
    query: &str,
    text_of: impl Fn(&T) -> String,
) -> Vec<&'a T> {
    let query = query.trim();
    if query.is_empty() {
        return items.iter().collect();
    }
    let tokens: Vec<&str> = query
        .split(|character: char| character.is_whitespace() || character == '/')
        .filter(|token| !token.is_empty())
        .collect();
    if tokens.is_empty() {
        return items.iter().collect();
    }
    let mut results: Vec<(&'a T, i64)> = items
        .iter()
        .filter_map(|item| {
            let text = text_of(item);
            let mut total = 0i64;
            for token in &tokens {
                total += fuzzy_match(token, &text)?;
            }
            Some((item, total))
        })
        .collect();
    results.sort_by_key(|(_, score)| *score);
    results.into_iter().map(|(item, _)| item).collect()
}

pub(crate) fn search_files(query: &str, limit: usize) -> Vec<String> {
    let Ok(output) = std::process::Command::new("git")
        .args(["ls-files", "-co", "--exclude-standard", "-z"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let query = query.to_ascii_lowercase();
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter_map(|path| std::str::from_utf8(path).ok())
        .filter(|path| path.to_ascii_lowercase().contains(&query))
        .take(limit)
        .map(str::to_string)
        .collect()
}

pub(crate) fn blink_cursor(start: Instant) -> &'static str {
    if (start.elapsed().as_millis() / 500).is_multiple_of(2) {
        "▏"
    } else {
        " "
    }
}

pub(crate) fn load_template(path: Option<&std::ffi::OsStr>) -> anyhow::Result<Template> {
    match path {
        Some(path) => Template::from_path(PathBuf::from(path)).map_err(|e| anyhow::anyhow!("{e}")),
        None => Ok(Template::from_source(include_str!("../shell.crepus"))),
    }
}

pub(crate) struct ChatMessage {
    pub(crate) role: String,
    pub(crate) content: String,
    pub(crate) is_tool: bool,
    pub(crate) tool_name: String,
    pub(crate) tool_call_id: String,
    pub(crate) is_streaming: bool,
}

#[derive(Clone)]
pub(crate) struct ConfiguredProvider {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) client: Arc<dyn Provider>,
}

#[derive(Clone)]
pub(crate) struct ModelChoice {
    pub(crate) id: String,
    pub(crate) provider: String,
}

pub(crate) struct App {
    pub(crate) input: String,
    /// Character index of the edit cursor inside `input` (pi-style editing).
    pub(crate) cursor: usize,
    /// (input, cursor) snapshots before each edit for Ctrl+Z undo (pi-style).
    pub(crate) undo_stack: Vec<(String, usize)>,
    pub(crate) pastes: Vec<String>,
    pub(crate) messages: Vec<ChatMessage>,
    pub(crate) model: String,
    pub(crate) effort: String,
    pub(crate) model_choices: Vec<ModelChoice>,
    pub(crate) model_context_windows: HashMap<String, usize>,
    pub(crate) model_registry: ModelRegistry,
    pub(crate) model_choice: Option<usize>,
    pub(crate) selecting_model: bool,
    pub(crate) providers: Vec<ConfiguredProvider>,
    pub(crate) provider_choice: usize,
    pub(crate) busy: bool,
    pub(crate) auto_scroll: bool,
    pub(crate) input_history: Vec<String>,
    pub(crate) history_index: Option<usize>,
    pub(crate) history_draft: String,
    pub(crate) file_suggestions: Vec<String>,
    pub(crate) file_choice: usize,
    pub(crate) pending_file_query: Option<String>,
    /// Debounce deadline for the in-flight `@` file search.
    pub(crate) file_search_deadline: Option<Instant>,
    /// Last JSONL session flush, for throttling appends.
    pub(crate) last_persist: Option<Instant>,
    pub(crate) slash_suggestions: Vec<String>,
    pub(crate) slash_choice: usize,
    pub(crate) input_tokens: usize,
    pub(crate) output_tokens: usize,
    pub(crate) cache_read_tokens: usize,
    pub(crate) cache_write_tokens: usize,
    pub(crate) cost: f64,
    /// rx4 keeps a session-wide cost total; this baseline makes `/clear`
    /// reset the host view without duplicating pricing logic.
    pub(crate) cost_baseline: f64,
    pub(crate) spinner_start: Instant,
    pub(crate) cursor_start: Instant,
    pub(crate) show_header: bool,
    pub(crate) permission_prompt: bool,
    pub(crate) permission_tool: String,
    /// Ones-shot reply channel while UI waits for y/n.
    pub(crate) permission_respond: Option<std::sync::mpsc::SyncSender<Decision>>,
    /// Whole-turn plan approval gate (rx4 owns the blocking wait).
    pub(crate) plan_prompt: bool,
    pub(crate) plan_rows: Vec<String>,
    pub(crate) plan_respond: Option<tokio::sync::oneshot::Sender<PlanDecision>>,
    pub(crate) session_name: String,
    pub(crate) context_pct: usize,
    pub(crate) context_tokens: usize,
    pub(crate) context_window: usize,
    pub(crate) agent: Option<Arc<Mutex<Agent>>>,
    pub(crate) cancellation: Option<CancellationHandle>,
    pub(crate) cancellation_requested: bool,
    pub(crate) event_rx: Option<tokio::sync::mpsc::UnboundedReceiver<AppEvent>>,
    pub(crate) approval_rx: Option<std::sync::mpsc::Receiver<PendingApproval>>,
    pub(crate) plan_rx: Option<tokio::sync::mpsc::Receiver<PendingPlanApproval>>,
    pub(crate) approval_mode: Option<ApprovalMode>,
    /// Interactive `/config` menu state.
    pub(crate) config_open: bool,
    pub(crate) config_choice: usize,
    /// Searchable provider/API-key catalog, distinct from runtime config.
    pub(crate) provider_menu_open: bool,
    pub(crate) provider_catalog_choice: usize,
    /// Only the live TUI persists prefs; `App::new()` (tests) leaves them alone.
    pub(crate) prefs_enabled: bool,
    pub(crate) prompt_char: String,
    pub(crate) agent_mode: String,
    /// Fully-qualified MCP tool names registered at startup (`mcp__server__tool`).
    pub(crate) mcp_tools: Vec<String>,
    pub(crate) mcp_connecting: bool,
    pub(crate) subagent_manager: Option<Arc<ParkingMutex<SubagentManager>>>,
    pub(crate) project: String,
    pub(crate) branch: String,
    pub(crate) branch_checked: Option<Instant>,
    #[cfg(feature = "pi-compat")]
    pub(crate) session: Option<(PiSession, PathBuf)>,
}

pub(crate) enum AppEvent {
    Rx4(Rx4Event),
    Error(String),
    PromptFailed {
        prompt: String,
    },
    FileSuggestions {
        query: String,
        paths: Vec<String>,
    },
    ModelChoices {
        choices: Vec<ModelChoice>,
        context_windows: HashMap<String, usize>,
        models: Vec<ModelInfo>,
    },
    Idle,
}

pub(crate) type PendingPlanApproval = (PlanProposal, tokio::sync::oneshot::Sender<PlanDecision>);

impl App {
    pub(crate) fn new() -> Self {
        Self {
            input: String::new(),
            cursor: 0,
            undo_stack: Vec::new(),
            pastes: Vec::new(),
            messages: Vec::new(),
            model: "no-model".to_string(),
            effort: "high".to_string(),
            model_choices: Vec::new(),
            model_context_windows: HashMap::new(),
            model_registry: ModelRegistry::new(),
            model_choice: None,
            selecting_model: false,
            providers: Vec::new(),
            provider_choice: 0,
            busy: false,
            auto_scroll: true,
            input_history: load_history(),
            history_index: None,
            history_draft: String::new(),
            file_suggestions: Vec::new(),
            file_choice: 0,
            pending_file_query: None,
            file_search_deadline: None,
            last_persist: None,
            slash_suggestions: Vec::new(),
            slash_choice: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost: 0.0,
            cost_baseline: 0.0,
            spinner_start: Instant::now(),
            cursor_start: Instant::now(),
            show_header: false,
            permission_prompt: false,
            permission_tool: String::new(),
            permission_respond: None,
            plan_prompt: false,
            plan_rows: Vec::new(),
            plan_respond: None,
            session_name: "default".to_string(),
            context_pct: 0,
            context_tokens: 0,
            context_window: 128_000,
            agent: None,
            cancellation: None,
            cancellation_requested: false,
            event_rx: None,
            approval_rx: None,
            plan_rx: None,
            approval_mode: None,
            config_open: false,
            config_choice: 0,
            provider_menu_open: false,
            provider_catalog_choice: 0,
            prefs_enabled: false,
            prompt_char: ">".to_string(),
            agent_mode: "coding".to_string(),
            mcp_tools: Vec::new(),
            mcp_connecting: false,
            subagent_manager: None,
            project: project_name(),
            branch: "-".to_string(),
            branch_checked: None,
            #[cfg(feature = "pi-compat")]
            session: None,
        }
    }

    pub(crate) fn refresh_branch(&mut self) {
        if self
            .branch_checked
            .is_none_or(|checked| checked.elapsed() >= std::time::Duration::from_secs(5))
        {
            self.branch = git_branch();
            self.branch_checked = Some(Instant::now());
        }
    }

    pub(crate) fn clear_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
        self.pastes.clear();
    }

    pub(crate) fn expanded_input(&self) -> String {
        self.pastes
            .iter()
            .enumerate()
            .fold(self.input.clone(), |input, (index, paste)| {
                input.replace(&format!("[paste #{}]", index + 1), paste)
            })
    }

    /// Byte offset of the character-indexed cursor (pi-style editing).
    pub(crate) fn cursor_byte(&self) -> usize {
        self.input
            .char_indices()
            .nth(self.cursor)
            .map(|(byte, _)| byte)
            .unwrap_or(self.input.len())
    }

    /// Remember the pre-edit state for Ctrl+Z undo (pi's undo-stack).
    pub(crate) fn snapshot_undo(&mut self) {
        if self
            .undo_stack
            .last()
            .is_some_and(|(input, cursor)| *input == self.input && *cursor == self.cursor)
        {
            return;
        }
        self.undo_stack.push((self.input.clone(), self.cursor));
        if self.undo_stack.len() > 50 {
            self.undo_stack.remove(0);
        }
    }

    pub(crate) fn undo(&mut self) {
        if let Some((input, cursor)) = self.undo_stack.pop() {
            self.input = input;
            self.cursor = cursor;
        }
    }

    pub(crate) fn cursor_to_start(&mut self) {
        self.cursor = 0;
    }

    pub(crate) fn cursor_to_end(&mut self) {
        self.cursor = self.input.chars().count();
    }

    pub(crate) fn move_cursor(&mut self, delta: isize) {
        let len = self.input.chars().count() as isize;
        self.cursor = (self.cursor as isize + delta).clamp(0, len) as usize;
    }

    pub(crate) fn insert_at_cursor(&mut self, text: &str) {
        self.snapshot_undo();
        let byte = self.cursor_byte();
        self.input.insert_str(byte, text);
        self.cursor += text.chars().count();
    }

    pub(crate) fn delete_back_at_cursor(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.snapshot_undo();
        let byte = self.cursor_byte();
        let start = self.input[..byte]
            .char_indices()
            .next_back()
            .map(|(index, _)| index)
            .unwrap_or(0);
        self.input.replace_range(start..byte, "");
        self.cursor -= 1;
    }

    pub(crate) fn delete_forward_at_cursor(&mut self) {
        let byte = self.cursor_byte();
        if byte == self.input.len() {
            return;
        }
        self.snapshot_undo();
        let end = self.input[byte..]
            .char_indices()
            .nth(1)
            .map(|(index, _)| byte + index)
            .unwrap_or(self.input.len());
        self.input.replace_range(byte..end, "");
    }

    pub(crate) fn move_word(&mut self, delta: isize) {
        let chars: Vec<char> = self.input.chars().collect();
        let mut position = self.cursor as isize;
        if delta > 0 {
            while (position as usize) < chars.len() && !chars[position as usize].is_whitespace() {
                position += 1;
            }
            while (position as usize) < chars.len() && chars[position as usize].is_whitespace() {
                position += 1;
            }
        } else {
            if position > 0 {
                position -= 1;
            }
            while position > 0 && chars[position as usize].is_whitespace() {
                position -= 1;
            }
            while position > 0 && !chars[position as usize - 1].is_whitespace() {
                position -= 1;
            }
        }
        self.cursor = position.clamp(0, chars.len() as isize) as usize;
    }

    pub(crate) fn delete_word_back(&mut self) {
        let before = self.cursor;
        self.move_word(-1);
        let after = self.cursor;
        if after == before {
            return;
        }
        self.snapshot_undo();
        let start = self
            .input
            .char_indices()
            .nth(after)
            .map(|(index, _)| index)
            .unwrap_or(self.input.len());
        let end = self
            .input
            .char_indices()
            .nth(before)
            .map(|(index, _)| index)
            .unwrap_or(self.input.len());
        self.input.replace_range(start..end, "");
        self.cursor = after;
    }

    pub(crate) fn delete_to_start(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.snapshot_undo();
        let byte = self.cursor_byte();
        self.input.replace_range(..byte, "");
        self.cursor = 0;
    }

    pub(crate) fn delete_to_end(&mut self) {
        let byte = self.cursor_byte();
        if byte == self.input.len() {
            return;
        }
        self.snapshot_undo();
        self.input.truncate(byte);
    }

    pub(crate) fn paste(&mut self, pasted: &str) {
        let pasted: String = pasted
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .chars()
            .filter(|character| *character == '\n' || !character.is_control())
            .collect();
        if pasted.is_empty() {
            return;
        }
        if pasted.split('\n').count() > LARGE_PASTE_LINES
            || pasted.chars().count() > LARGE_PASTE_CHARS
        {
            self.pastes.push(pasted);
            self.insert_at_cursor(&format!("[paste #{}]", self.pastes.len()));
        } else {
            self.insert_at_cursor(&pasted);
        }
        self.file_suggestions.clear();
        self.pending_file_query = None;
    }

    pub(crate) fn insert_newline(&mut self) {
        self.insert_at_cursor("\n");
        self.file_suggestions.clear();
        self.pending_file_query = None;
        self.slash_suggestions.clear();
    }

    #[cfg(feature = "pi-compat")]
    pub(crate) fn persist(&mut self) -> std::io::Result<()> {
        if let Some((session, dir)) = &mut self.session {
            session.save_jsonl(dir)?;
        }
        Ok(())
    }

    #[cfg(feature = "pi-compat")]
    pub(crate) fn persist_with_error(&mut self) {
        if let Err(error) = self.persist() {
            self.messages.push(ChatMessage {
                role: "error".to_string(),
                content: format!("Session save failed: {error}"),
                is_tool: false,
                tool_name: String::new(),
                tool_call_id: String::new(),
                is_streaming: false,
            });
        }
    }

    #[cfg(feature = "pi-compat")]
    pub(crate) fn append_session(&mut self, entry: PiEntryType) {
        if let Some((session, _)) = &mut self.session {
            session.append(entry);
            // Throttle fsync-heavy JSONL appends: entries stay buffered in
            // memory and a final flush happens on turn end (Idle) and exit.
            let due = self
                .last_persist
                .is_none_or(|last| last.elapsed() >= SESSION_PERSIST_INTERVAL);
            if !due {
                return;
            }
            self.last_persist = Some(Instant::now());
            self.persist_with_error();
        }
    }

    /// Force-write any buffered session entries (turn end / exit).
    #[cfg(feature = "pi-compat")]
    pub(crate) fn flush_session(&mut self) {
        self.last_persist = Some(Instant::now());
        self.persist_with_error();
    }

    pub(crate) fn poll_pending_approvals(&mut self) {
        let Some(rx) = self.approval_rx.as_ref() else {
            return;
        };
        while let Ok(pending) = rx.try_recv() {
            self.permission_prompt = true;
            let detail = tool_detail(&pending.tool_name, &pending.arguments);
            self.permission_tool = if detail.is_empty() {
                pending.tool_name
            } else {
                format!("{} {detail}", pending.tool_name)
            };
            self.permission_respond = Some(pending.respond);
        }
    }

    pub(crate) fn poll_pending_plan_approvals(&mut self) {
        let Some(rx) = self.plan_rx.as_mut() else {
            return;
        };
        while let Ok((proposal, respond)) = rx.try_recv() {
            self.plan_prompt = true;
            self.plan_rows = bounded_plan_preview(&proposal);
            self.plan_respond = Some(respond);
        }
    }

    pub(crate) fn resolve_permission(&mut self, allow: bool) {
        if let Some(tx) = self.permission_respond.take() {
            let _ = tx.send(if allow {
                Decision::Allow
            } else {
                Decision::Deny
            });
        }
        self.permission_prompt = false;
        self.permission_tool.clear();
    }

    pub(crate) fn resolve_plan(&mut self, approve: bool) {
        if let Some(tx) = self.plan_respond.take() {
            let decision = if approve {
                PlanDecision::Approve
            } else {
                PlanDecision::Reject("rejected by user".to_string())
            };
            let _ = tx.send(decision);
        }
        self.plan_prompt = false;
        self.plan_rows.clear();
    }

    pub(crate) fn cancel_turn(&mut self) {
        if !self.busy {
            return;
        }
        if self.permission_prompt {
            self.resolve_permission(false);
        }
        if self.plan_prompt {
            self.resolve_plan(false);
        }
        self.cancellation_requested = true;
        if let Some(cancellation) = &self.cancellation {
            cancellation.cancel();
        }
        for message in &mut self.messages {
            message.is_streaming = false;
        }
    }

    pub(crate) fn toggle_permission_mode(&mut self) {
        let Some(mode) = &self.approval_mode else {
            return;
        };
        if mode.toggle() && self.permission_prompt {
            self.resolve_permission(true);
        }
    }

    pub(crate) fn refresh_cost(&mut self) {
        let total = self
            .agent
            .as_ref()
            .and_then(|agent| agent.try_lock().ok().map(|agent| agent.total_cost()));
        if let Some(total) = total {
            self.cost = (total - self.cost_baseline).max(0.0);
        }
    }

    pub(crate) fn update_template(&mut self, tpl: &mut Template) {
        self.refresh_cost();
        tpl.set("input", self.input.clone());
        tpl.set("input_len", self.input.chars().count() as i64);
        let cursor_byte = self.cursor_byte();
        tpl.set("input_before", self.input[..cursor_byte].to_string());
        tpl.set("input_after", self.input[cursor_byte..].to_string());
        tpl.set("model", self.model.clone());
        tpl.set("effort", self.effort.clone());
        tpl.set("input_color", effort_color(&self.effort));
        tpl.set("selecting_model", self.selecting_model);
        tpl.set("config_open", self.config_open);
        tpl.set("provider_menu_open", self.provider_menu_open);
        let config_rows = if self.config_open {
            self.config_menu_rows()
                .into_iter()
                .enumerate()
                .map(|(index, (label, hint))| {
                    let mut row = TemplateContext::new();
                    row.set("label", label);
                    row.set("hint", hint);
                    row.set("selected", index == self.config_choice);
                    row
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        tpl.set("config_rows", TemplateValue::List(config_rows));
        let provider_rows = if self.provider_menu_open {
            self.filtered_provider_catalog()
                .into_iter()
                .enumerate()
                .skip(self.provider_catalog_choice.saturating_sub(3))
                .take(7)
                .map(|(index, provider)| {
                    let mut row = TemplateContext::new();
                    row.set("name", provider.name);
                    row.set("id", provider.id);
                    row.set("env", provider.env);
                    row.set("configured", provider_catalog::env_key(provider).is_some());
                    row.set("selected", index == self.provider_catalog_choice);
                    row
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        tpl.set("provider_rows", TemplateValue::List(provider_rows));
        tpl.set(
            "selected_provider",
            self.providers
                .get(self.provider_choice)
                .map(|provider| provider.name.clone())
                .unwrap_or_default(),
        );
        // Only the model selector renders these; skip the fuzzy work otherwise.
        let filtered_models = if self.selecting_model {
            self.filtered_models()
        } else {
            Vec::new()
        };
        tpl.set(
            "no_model_matches",
            self.selecting_model && !self.input.trim().is_empty() && filtered_models.is_empty(),
        );
        tpl.set(
            "selected_model",
            self.model_choice
                .and_then(|index| filtered_models.get(index).map(|model| model.id.clone()))
                .unwrap_or_default(),
        );
        let model_rows = if self.selecting_model {
            filtered_models
                .into_iter()
                .enumerate()
                .skip(self.model_choice.unwrap_or_default().saturating_sub(2))
                .take(5)
                .map(|(index, model)| {
                    let mut row = TemplateContext::new();
                    row.set("model_id", model.id.clone());
                    row.set("selected", Some(index) == self.model_choice);
                    row
                })
                .collect()
        } else {
            Vec::new()
        };
        tpl.set("model_rows", TemplateValue::List(model_rows));
        let file_rows = if self.file_suggestions.is_empty() {
            Vec::new()
        } else {
            self.file_suggestions
                .iter()
                .enumerate()
                .map(|(index, path)| {
                    let mut row = TemplateContext::new();
                    row.set("path", path.clone());
                    row.set("selected", index == self.file_choice);
                    row
                })
                .collect()
        };
        tpl.set("has_file_suggestions", !self.file_suggestions.is_empty());
        tpl.set("file_rows", TemplateValue::List(file_rows));
        let slash_rows = if self.slash_suggestions.is_empty() {
            Vec::new()
        } else {
            self.slash_suggestions
                .iter()
                .enumerate()
                .map(|(index, command)| {
                    let mut row = TemplateContext::new();
                    row.set("command", command.clone());
                    row.set("desc", self.slash_row_description(command));
                    row.set("selected", index == self.slash_choice);
                    row
                })
                .collect()
        };
        tpl.set("has_slash_suggestions", !self.slash_suggestions.is_empty());
        tpl.set("slash_rows", TemplateValue::List(slash_rows));
        tpl.set("busy", self.busy);
        tpl.set("auto_scroll", self.auto_scroll);
        tpl.set("version", env!("CARGO_PKG_VERSION"));
        tpl.set("session_name", self.session_name.clone());
        tpl.set("show_header", self.show_header);
        tpl.set(
            "spinner",
            if self.busy {
                spinner_frame(self.spinner_start)
            } else {
                ""
            },
        );
        tpl.set("cursor", blink_cursor(self.cursor_start));
        tpl.set("prompt_char", self.prompt_char.clone());
        tpl.set("agent_mode", self.agent_mode.clone());
        tpl.set("permission_prompt", self.permission_prompt);
        tpl.set("permission_tool", self.permission_tool.clone());
        tpl.set("plan_prompt", self.plan_prompt);
        let plan_rows = self
            .plan_rows
            .iter()
            .map(|line| {
                let mut row = TemplateContext::new();
                row.set("text", line.clone());
                row
            })
            .collect::<Vec<_>>();
        tpl.set("plan_rows", TemplateValue::List(plan_rows));
        tpl.set(
            "permission_mode",
            self.approval_mode.as_ref().map_or("bypass", |mode| {
                if mode.is_bypass() {
                    "bypass"
                } else {
                    "ask"
                }
            }),
        );
        tpl.set("project", self.project.clone());
        tpl.set("branch", self.branch.clone());
        tpl.set("cost", format!("{:.3}", self.cost));
        tpl.set("context_pct", self.context_pct.to_string());
        tpl.set("context_window", format_tokens(self.context_window));
        tpl.set("context_color", context_color(self.context_pct));
        let running_subagents = self
            .subagent_manager
            .as_ref()
            .map(|manager| {
                let manager = manager.lock();
                manager
                    .list()
                    .iter()
                    .filter(|handle| {
                        matches!(
                            handle.status(),
                            SubagentStatus::Pending | SubagentStatus::Running
                        )
                    })
                    .map(|handle| {
                        let mut subagent = TemplateContext::new();
                        subagent.set("name", handle.name().to_string());
                        subagent
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        tpl.set("has_running_subagents", !running_subagents.is_empty());
        tpl.set("running_subagents", TemplateValue::List(running_subagents));

        let msgs: Vec<TemplateContext> = self
            .messages
            .iter()
            .map(|m| {
                let mut mc = TemplateContext::new();
                mc.set("is_user", m.role == "user");
                mc.set("is_tool", m.is_tool);
                mc.set("tool_name", m.tool_name.clone());
                mc.set("is_streaming", m.is_streaming);
                // A streaming assistant message with no text yet is "thinking":
                // surface it as a trail instead of a bare, duplicate caret.
                mc.set(
                    "is_thinking",
                    m.is_streaming && !m.is_tool && m.content.trim().is_empty(),
                );
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

    pub(crate) fn submit_prompt(
        &mut self,
        agent: &Arc<Mutex<Agent>>,
        tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
    ) {
        let text = self.expanded_input().trim().to_string();
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
            tool_call_id: String::new(),
            is_streaming: false,
        });
        #[cfg(feature = "pi-compat")]
        self.append_session(PiEntryType::Message {
            role: Role::User,
            content: text.clone(),
            tool_call_id: None,
        });

        self.clear_input();
        self.cancellation_requested = false;
        self.busy = true;

        let agent = agent.clone();
        tokio::spawn(async move {
            let mut agent = agent.lock().await;
            let result = agent.prompt(&text).await;
            if let Err(error) = result {
                if !matches!(error, AgentError::Cancelled) {
                    let _ = tx.send(AppEvent::PromptFailed { prompt: text });
                }
            }
            let _ = tx.send(AppEvent::Idle);
        });
    }

    pub(crate) fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Rx4(e) => self.handle_rx4_event(e),
            AppEvent::Error(msg) => {
                self.messages.push(ChatMessage {
                    role: "error".to_string(),
                    content: format!("Error: {msg}"),
                    is_tool: false,
                    tool_name: String::new(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
            }
            AppEvent::PromptFailed { prompt } => {
                if self.input.is_empty() {
                    self.input = prompt;
                    self.cursor_to_end();
                }
                self.file_suggestions.clear();
                self.pending_file_query = None;
            }
            AppEvent::FileSuggestions { query, paths } => {
                if file_query(&self.input) == Some(query.as_str()) {
                    self.file_suggestions = paths;
                    self.file_choice = 0;
                }
                if self.pending_file_query.as_deref() == Some(query.as_str()) {
                    self.pending_file_query = None;
                }
            }
            AppEvent::ModelChoices {
                choices,
                context_windows,
                models,
            } => {
                self.model_choices.extend(choices);
                self.model_choices
                    .sort_by(|a, b| a.provider.cmp(&b.provider).then(a.id.cmp(&b.id)));
                self.model_choices
                    .dedup_by(|a, b| a.provider == b.provider && a.id == b.id);
                self.model_context_windows.extend(context_windows);
                self.model_registry.extend(models);
                if let Some(agent) = &self.agent {
                    if let Ok(mut agent) = agent.try_lock() {
                        agent.set_model_registry(self.model_registry.clone());
                    }
                }
                if self.selecting_model {
                    self.reset_model_choice();
                }
            }
            AppEvent::Idle => {
                self.busy = false;
                #[cfg(feature = "pi-compat")]
                self.flush_session();
            }
        }
    }

    pub(crate) fn refresh_file_suggestions(&mut self) {
        let Some(query) = file_query(&self.input).map(str::to_string) else {
            self.file_suggestions.clear();
            self.pending_file_query = None;
            self.file_search_deadline = None;
            return;
        };
        if self.pending_file_query.as_deref() == Some(query.as_str()) {
            return;
        }
        // Debounce: typing "@src/mai" spawns one `git ls-files` after the
        // typing settles instead of one process per keystroke.
        self.pending_file_query = Some(query);
        self.file_search_deadline = Some(Instant::now() + FILE_SEARCH_DEBOUNCE);
    }

    /// Called from the main loop each tick; runs the debounced file search once
    /// its quiet window has elapsed.
    pub(crate) fn maybe_run_file_search(
        &mut self,
        tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
    ) {
        let Some(deadline) = self.file_search_deadline else {
            return;
        };
        if Instant::now() < deadline {
            return;
        }
        self.file_search_deadline = None;
        let Some(query) = self.pending_file_query.take() else {
            return;
        };
        tokio::task::spawn_blocking(move || {
            let paths = search_files(&query, 8);
            let _ = tx.send(AppEvent::FileSuggestions { query, paths });
        });
    }

    pub(crate) fn move_file_choice(&mut self, delta: isize) {
        if self.file_suggestions.is_empty() {
            return;
        }
        self.file_choice = (self.file_choice as isize + delta)
            .rem_euclid(self.file_suggestions.len() as isize) as usize;
    }

    pub(crate) fn refresh_slash_suggestions(&mut self) {
        // `/model <partial>` gets pi-style argument completion: fuzzy model
        // ids across every configured provider, shown as full commands.
        if let Some(arg) = self
            .input
            .strip_prefix("/model ")
            .filter(|arg| !arg.is_empty())
        {
            self.slash_suggestions = fuzzy_filter(&self.model_choices, arg, |model| {
                format!("{} {}/{}", model.provider, model.provider, model.id)
            })
            .into_iter()
            .take(8)
            .map(|model| format!("/model {}", model.id))
            .collect();
        } else {
            self.slash_suggestions = matching_slash_commands(&self.input);
        }
        self.slash_choice = 0;
    }

    /// Description shown next to a slash suggestion (pi-style autocomplete):
    /// command descriptions for commands, provider names for model arguments.
    pub(crate) fn slash_row_description(&self, suggestion: &str) -> String {
        if let Some(arg) = suggestion
            .strip_prefix("/model ")
            .filter(|arg| !arg.is_empty())
        {
            return self
                .model_choices
                .iter()
                .find(|model| model.id == arg)
                .map(|model| model.provider.clone())
                .unwrap_or_else(|| "model".to_string());
        }
        slash_description(suggestion).to_string()
    }

    pub(crate) fn move_slash_choice(&mut self, delta: isize) {
        if self.slash_suggestions.is_empty() {
            return;
        }
        self.slash_choice = (self.slash_choice as isize + delta)
            .rem_euclid(self.slash_suggestions.len() as isize) as usize;
    }

    pub(crate) fn choose_slash_command(&mut self) {
        let Some(command) = self.slash_suggestions.get(self.slash_choice).cloned() else {
            return;
        };
        self.snapshot_undo();
        self.input = format!("{command} ");
        self.cursor_to_end();
        self.slash_suggestions.clear();
    }

    pub(crate) fn dismiss_suggestions(&mut self) {
        self.slash_suggestions.clear();
        self.file_suggestions.clear();
        self.pending_file_query = None;
        self.file_search_deadline = None;
    }

    pub(crate) fn choose_file(&mut self) {
        let Some(path) = self.file_suggestions.get(self.file_choice).cloned() else {
            return;
        };
        let Some(query) = file_query(&self.input).map(str::to_string) else {
            return;
        };
        let start = self.input.len() - query.len();
        self.snapshot_undo();
        self.input.replace_range(start.., &path);
        self.input.push(' ');
        self.cursor_to_end();
        self.file_suggestions.clear();
        self.pending_file_query = None;
    }

    pub(crate) fn handle_rx4_event(&mut self, event: Rx4Event) {
        match event {
            Rx4Event::AgentStart => {}
            Rx4Event::ContextUsage {
                used_tokens,
                context_window,
                ..
            } => {
                self.context_window = context_window;
                self.context_tokens = used_tokens;
                self.refresh_context_pct();
            }
            Rx4Event::Usage { usage, .. } => {
                self.input_tokens += usage.input_tokens;
                self.output_tokens += usage.output_tokens;
                self.cache_read_tokens += usage.cache_read_tokens;
                self.cache_write_tokens += usage.cache_write_tokens;
            }
            Rx4Event::CompactionStart { .. } => {
                self.messages.push(ChatMessage {
                    role: "tool".to_string(),
                    content: String::new(),
                    is_tool: true,
                    tool_name: "compacting context".to_string(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
            }
            Rx4Event::CompactionEnd { result, .. } => {
                self.messages.push(ChatMessage {
                    role: "tool".to_string(),
                    content: format!("{} tokens remain", result.remaining_tokens),
                    is_tool: true,
                    tool_name: "compacted context".to_string(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
                #[cfg(feature = "pi-compat")]
                self.append_session(PiEntryType::Compaction {
                    summary: result.summary,
                    cut_at: result.removed_count,
                });
            }
            Rx4Event::SkillActivated { name, .. } => {
                self.messages.push(ChatMessage {
                    role: "tool".to_string(),
                    content: String::new(),
                    is_tool: true,
                    tool_name: format!("skill {name}"),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
            }
            Rx4Event::ToolSource { tool, source } => {
                let activity = match source {
                    ToolSource::Builtin => None,
                    ToolSource::Mcp { server } => Some(format!("used {server} (MCP)")),
                    ToolSource::ComputerUse => Some(format!("used {tool}")),
                };
                if let Some(tool_name) = activity {
                    self.messages.push(ChatMessage {
                        role: "tool".to_string(),
                        content: String::new(),
                        is_tool: true,
                        tool_name,
                        tool_call_id: String::new(),
                        is_streaming: false,
                    });
                }
            }
            Rx4Event::TurnStart { .. } => {
                self.messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: String::new(),
                    is_tool: false,
                    tool_name: String::new(),
                    tool_call_id: String::new(),
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
                        tool_call_id: String::new(),
                        is_streaming: true,
                    });
                }
            }
            Rx4Event::MessageDelta { delta } => {
                if let Some(msg) = self
                    .messages
                    .iter_mut()
                    .rev()
                    .find(|message| message.role == "assistant" && message.is_streaming)
                {
                    msg.content.push_str(&delta);
                }
            }
            Rx4Event::MessageEnd { content, .. } => {
                if let Some(msg) = self
                    .messages
                    .iter_mut()
                    .rev()
                    .find(|message| message.role == "assistant" && message.is_streaming)
                {
                    if !content.is_empty() {
                        msg.content = content.clone();
                    }
                    msg.is_streaming = false;
                }
                if !content.is_empty() {
                    #[cfg(feature = "pi-compat")]
                    self.append_session(PiEntryType::Message {
                        role: Role::Assistant,
                        content,
                        tool_call_id: None,
                    });
                }
            }
            Rx4Event::ToolCall(call) => {
                #[cfg(feature = "pi-compat")]
                self.append_session(PiEntryType::Custom {
                    extension: "telekinesis.tool_call".to_string(),
                    payload: serde_json::json!({
                        "id": &call.id,
                        "name": &call.name,
                        "arguments": &call.arguments,
                    }),
                });
                self.messages.push(ChatMessage {
                    role: "tool".to_string(),
                    content: tool_detail(&call.name, &call.arguments),
                    is_tool: true,
                    tool_name: call.name,
                    tool_call_id: call.id,
                    is_streaming: true,
                });
            }
            Rx4Event::ApprovalRequired(_) => {}
            Rx4Event::ToolExecutionStart(_) => {}
            Rx4Event::ToolExecutionEnd(result) => {
                if let Some(msg) = self.messages.iter_mut().rev().find(|message| {
                    message.is_tool && message.is_streaming && message.tool_call_id == result.id
                }) {
                    let detail = std::mem::take(&mut msg.content);
                    let summary =
                        tool_result_summary(&msg.tool_name, &result.content, result.is_error);
                    msg.content = if detail.is_empty() {
                        summary
                    } else {
                        format!("{detail} → {summary}")
                    };
                    msg.role = if result.is_error { "error" } else { "tool" }.to_string();
                    msg.is_streaming = false;
                }
                #[cfg(feature = "pi-compat")]
                self.append_session(PiEntryType::Custom {
                    extension: "telekinesis.tool_result".to_string(),
                    payload: serde_json::json!({
                        "id": &result.id,
                        "content": &result.content,
                        "is_error": result.is_error,
                    }),
                });
            }
            Rx4Event::TurnEnd { .. } => {}
            // Optional rx4 0.6.4 host-observability events. The detailed UI
            // wiring follows separately; keeping them no-op preserves the
            // existing transcript behaviour while accepting the additive API.
            Rx4Event::TodoUpdated { .. }
            | Rx4Event::TurnEnded { .. }
            | Rx4Event::CacheAudit(_)
            | Rx4Event::GateResult(_)
            | Rx4Event::MemoryRecalled { .. } => {}
            Rx4Event::AgentEnd => {
                if let Some(msg) = self.messages.last_mut() {
                    msg.is_streaming = false;
                }
            }
            // rx4 0.6.0 runs guardrails, self-healing and a plan gate inside
            // the loop. Surface them: a warning the user never sees is a
            // turn that changes behaviour for no visible reason.
            Rx4Event::GuardrailWarning { tool, reason } => {
                self.messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: format!("guardrail on `{tool}`: {reason}"),
                    is_tool: false,
                    tool_name: String::new(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
            }
            Rx4Event::GuardrailStop { tool, reason } => {
                self.messages.push(ChatMessage {
                    role: "error".to_string(),
                    content: format!("Stopped by guardrail on `{tool}`: {reason}"),
                    is_tool: false,
                    tool_name: String::new(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
                self.busy = false;
            }
            Rx4Event::SelfHealing {
                attempt,
                max_attempts,
                ..
            } => {
                self.messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: format!("retrying after a tool failure ({attempt}/{max_attempts})"),
                    is_tool: false,
                    tool_name: String::new(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
            }
            Rx4Event::PlanProposed(_) => {
                // The interactive proposal is rendered by `poll_pending_plan_approvals`.
                // Keeping the event itself quiet avoids duplicating the full plan in chat.
            }
            Rx4Event::PlanDecided { decision } => {
                let summary = match decision {
                    PlanDecision::Approve => {
                        "Plan approved; executing the proposed calls.".to_string()
                    }
                    PlanDecision::Reject(reason) => format!("Plan rejected: {reason}"),
                    PlanDecision::Revise(guidance) => {
                        format!("Plan revision requested: {guidance}")
                    }
                };
                self.messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: summary,
                    is_tool: false,
                    tool_name: String::new(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
            }
            Rx4Event::Error(msg) => {
                if self.cancellation_requested && msg.to_ascii_lowercase().contains("cancel") {
                    return;
                }
                self.messages.push(ChatMessage {
                    role: "error".to_string(),
                    content: format!("Error: {msg}"),
                    is_tool: false,
                    tool_name: String::new(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
            }
            Rx4Event::BudgetExceeded { reason } => {
                self.messages.push(ChatMessage {
                    role: "error".to_string(),
                    content: format!("Budget exceeded: {reason}"),
                    is_tool: false,
                    tool_name: String::new(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
            }
        }
    }

    pub(crate) fn history_get(&self) -> String {
        if let Some(idx) = self.history_index {
            self.input_history.get(idx).cloned().unwrap_or_default()
        } else {
            String::new()
        }
    }

    /// Rebuild the model catalog for every configured provider.
    ///
    /// The host-owned offline catalog is used until logged-in providers are
    /// refreshed from their live `/models` endpoints.
    pub(crate) fn refresh_model_choices(&mut self) {
        let mut registry = ModelRegistry::new();
        let mut context_windows = HashMap::new();
        let mut choices = Vec::new();
        let mut models = Vec::new();
        for provider in &self.providers {
            if let Some(spec) = provider_catalog::by_id(&provider.id) {
                for id in spec.models {
                    let model = host_model_info(&provider.id, id);
                    context_windows.insert(model.id.clone(), model.context_window);
                    choices.push(ModelChoice {
                        id: model.id.clone(),
                        provider: model.provider.clone(),
                    });
                    models.push(model);
                }
            }
        }
        if self
            .providers
            .iter()
            .any(|provider| provider.id == "openai-codex")
        {
            for id in rs_ai_oauth::codex::CHATGPT_CODEX_MODELS
                .iter()
                .chain(PI_CODEX_GPT56.iter())
            {
                let model = host_model_info("openai-codex", id);
                context_windows.insert(model.id.clone(), model.context_window);
                models.push(model.clone());
                choices.push(ModelChoice {
                    id: model.id,
                    provider: "openai-codex".to_string(),
                });
            }
        }
        // pi's current openai GPT-5.x family for the API-key provider.
        if self
            .providers
            .iter()
            .any(|provider| provider.id == "openai")
        {
            for id in PI_OPENAI_GPT5 {
                let model = host_model_info("openai", id);
                context_windows.insert(model.id.clone(), model.context_window);
                models.push(model.clone());
                choices.push(ModelChoice {
                    id: model.id,
                    provider: "openai".to_string(),
                });
            }
        }
        choices.sort_by(|a, b| a.provider.cmp(&b.provider).then(a.id.cmp(&b.id)));
        choices.dedup_by(|a, b| a.provider == b.provider && a.id == b.id);
        self.model_context_windows = context_windows;
        self.model_choices = choices;
        registry.extend(models.clone());
        self.model_registry = registry;
        if let Some(agent) = &self.agent {
            if let Ok(mut agent) = agent.try_lock() {
                agent.set_model_registry(self.model_registry.clone());
            }
        }
    }

    /// Refresh live OAuth and OpenRouter model metadata off the UI thread.
    /// A provider outage leaves the offline catalog intact.
    pub(crate) fn refresh_remote_model_choices(
        &self,
        tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
    ) {
        let provider_ids: Vec<String> = self.providers.iter().map(|p| p.id.clone()).collect();
        std::thread::spawn(move || {
            let mut choices = Vec::new();
            let mut context_windows = HashMap::new();
            let mut models = Vec::new();

            if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                if let Ok(provider_models) =
                    runtime.block_on(rs_ai_oauth::fetch_logged_in_models_async())
                {
                    for discovered in provider_models {
                        let Some(provider_id) =
                            configured_provider_id(discovered.provider, &provider_ids)
                        else {
                            continue;
                        };
                        for model in discovered.models {
                            if let Some(context_window) = model
                                .limits
                                .context_window
                                .and_then(|value| usize::try_from(value).ok())
                            {
                                context_windows.insert(model.id.clone(), context_window);
                            }
                            choices.push(ModelChoice {
                                id: model.id.clone(),
                                provider: provider_id.clone(),
                            });
                            models.push(oauth_model_info(&provider_id, model));
                        }
                    }
                }
            }

            if provider_ids.iter().any(|id| id == "openrouter") {
                if let Some(api_key) = std::env::var("OPENROUTER_API_KEY")
                    .ok()
                    .filter(|key| !key.is_empty())
                {
                    let response = reqwest::blocking::Client::builder()
                        .timeout(std::time::Duration::from_secs(8))
                        .build()
                        .and_then(|client| {
                            client
                                .get("https://openrouter.ai/api/v1/models")
                                .bearer_auth(api_key)
                                .send()
                        });
                    if let Ok(response) = response {
                        if response.status().is_success() {
                            if let Ok(value) = response.json::<serde_json::Value>() {
                                if let Some(data_models) =
                                    value.get("data").and_then(serde_json::Value::as_array)
                                {
                                    for model in data_models {
                                        let Some(id) =
                                            model.get("id").and_then(serde_json::Value::as_str)
                                        else {
                                            continue;
                                        };
                                        if let Some(context_window) = model
                                            .get("top_provider")
                                            .and_then(|provider| provider.get("context_length"))
                                            .or_else(|| model.get("context_length"))
                                            .and_then(serde_json::Value::as_u64)
                                            .and_then(|value| usize::try_from(value).ok())
                                        {
                                            context_windows.insert(id.to_string(), context_window);
                                        }
                                        choices.push(ModelChoice {
                                            id: id.to_string(),
                                            provider: "openrouter".to_string(),
                                        });
                                        if let Some(info) = openrouter_model_info(model) {
                                            models.push(info);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            let _ = tx.send(AppEvent::ModelChoices {
                choices,
                context_windows,
                models,
            });
        });
    }

    pub(crate) fn open_model_selector(&mut self) {
        self.refresh_model_choices();
        if let Some(choice) = self
            .model_choices
            .iter()
            .find(|choice| choice.id == self.model)
        {
            self.provider_choice = self
                .providers
                .iter()
                .position(|provider| provider.id == choice.provider)
                .unwrap_or(0);
        }
        self.clear_input();
        self.selecting_model = true;
        self.reset_model_choice();
    }

    pub(crate) fn filtered_models(&self) -> Vec<&ModelChoice> {
        let query = self.input.trim();
        // A query searches the whole catalog across every configured provider
        // (the provider rails collapse) with pi-style fuzzy ranking, matching
        // against `provider`, `provider/id`, and the bare id.
        if !query.is_empty() {
            return fuzzy_filter(&self.model_choices, query, |model| {
                format!(
                    "{} {}/{} {}",
                    model.provider, model.provider, model.id, model.id
                )
            });
        }
        let Some(provider) = self.providers.get(self.provider_choice) else {
            return Vec::new();
        };
        self.model_choices
            .iter()
            .filter(|model| model.provider == provider.id)
            .collect()
    }

    pub(crate) fn reset_model_choice(&mut self) {
        let choices = self.filtered_models();
        self.model_choice = choices
            .iter()
            .position(|model| model.id == self.model)
            .or((!choices.is_empty()).then_some(0));
    }

    pub(crate) fn move_provider_choice(&mut self, offset: isize) {
        if !self.selecting_model || self.providers.is_empty() {
            return;
        }
        let start = self.provider_choice;
        loop {
            self.provider_choice = (self.provider_choice as isize + offset)
                .rem_euclid(self.providers.len() as isize)
                as usize;
            self.reset_model_choice();
            if self.model_choice.is_some() || self.provider_choice == start {
                break;
            }
        }
    }

    pub(crate) fn move_model_choice(&mut self, offset: isize) {
        let Some(index) = self.model_choice else {
            return;
        };
        let len = self.filtered_models().len();
        if len != 0 {
            self.model_choice = Some((index as isize + offset).rem_euclid(len as isize) as usize);
        }
    }

    pub(crate) fn choose_model(&mut self) {
        let Some(index) = self.model_choice.take() else {
            return;
        };
        let Some(model) = self.filtered_models().get(index).cloned().cloned() else {
            return;
        };
        // When a search query is active the provider rails collapse, so resolve
        // the provider from the chosen model rather than the rail position.
        let provider = self
            .providers
            .iter()
            .find(|provider| provider.id == model.provider)
            .cloned()
            .or_else(|| self.providers.get(self.provider_choice).cloned())
            .expect("model choice always belongs to a configured provider");
        if let Some(index) = self
            .providers
            .iter()
            .position(|configured| configured.id == provider.id)
        {
            self.provider_choice = index;
        }
        #[cfg(feature = "pi-compat")]
        self.append_session(PiEntryType::ModelChange {
            from: self.model.clone(),
            to: model.id.clone(),
        });
        self.set_model(model.id.clone());
        self.selecting_model = false;
        self.clear_input();
        if let Some(agent) = &self.agent {
            if let Ok(mut agent) = agent.try_lock() {
                agent.set_provider(provider.client.clone());
                agent.set_model(model.id.clone());
            }
        }
        if let Some(manager) = &self.subagent_manager {
            let mut manager = manager.lock();
            manager.set_provider(provider.client);
            manager.set_model(model.id);
        }
    }

    pub(crate) fn set_model(&mut self, model: String) {
        self.model = model;
        self.context_window = self
            .model_context_windows
            .get(&self.model)
            .copied()
            .unwrap_or_else(|| context_window_for_model(&self.model));
        self.refresh_context_pct();
        self.persist_prefs();
    }

    pub(crate) fn persist_prefs(&self) {
        if !self.prefs_enabled {
            return;
        }
        save_prefs(&Prefs {
            model: Some(self.model.clone()),
            effort: Some(self.effort.clone()),
            scope: Some(self.agent_mode.clone()),
        });
    }

    pub(crate) fn refresh_context_pct(&mut self) {
        self.context_pct = self
            .context_tokens
            .saturating_mul(100)
            .checked_div(self.context_window)
            .unwrap_or(0);
    }

    pub(crate) fn cycle_effort(&mut self) {
        self.effort = match self.effort.as_str() {
            "low" => "medium",
            "medium" => "high",
            "high" => "xhigh",
            _ => "low",
        }
        .to_string();
        #[cfg(feature = "pi-compat")]
        self.append_session(PiEntryType::ThinkingLevelChange {
            level: self.effort.clone(),
        });
        if let Some(agent) = &self.agent {
            if let Ok(mut agent) = agent.try_lock() {
                agent.set_reasoning_effort(Some(self.effort.clone()));
            }
        }
        self.persist_prefs();
    }

    /// Cycle the agent scope (`coding → research → plan → ask → computer_use`)
    /// with a single keystroke, mirroring how `BackTab` cycles reasoning effort.
    pub(crate) fn cycle_scope(&mut self, offset: isize, agent: &Arc<Mutex<Agent>>) {
        let scopes = host::cycle_scopes();
        let current = self.agent_mode.as_str();
        let index = scopes
            .iter()
            .position(|scope| *scope == current)
            .unwrap_or(0);
        let next = scopes[(index as isize + offset).rem_euclid(scopes.len() as isize) as usize];
        if let Ok(scope) = host::parse_host_scope(next) {
            if let Ok(mut agent) = agent.try_lock() {
                host::apply_scope(&mut agent, scope);
            }
        }
        self.agent_mode = next.to_string();
        self.persist_prefs();
    }

    pub(crate) fn config_menu_rows(&self) -> Vec<(String, &'static str)> {
        vec![
            (format!("model · {}", self.model), "open model selector"),
            (
                format!("scope · {}", self.agent_mode),
                "cycle with the config menu or Alt+Shift+←/→",
            ),
            (
                format!("effort · {}", self.effort),
                "cycle reasoning effort",
            ),
            (
                format!("providers · {}", self.provider_names()),
                "log in with a new provider",
            ),
            (
                "show configuration".to_string(),
                "print the runtime summary",
            ),
        ]
    }
    pub(crate) fn provider_names(&self) -> String {
        if self.providers.is_empty() {
            "none".to_string()
        } else {
            self.providers
                .iter()
                .map(|provider| provider.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        }
    }

    pub(crate) fn open_config(&mut self) {
        self.config_open = true;
        self.config_choice = 0;
        self.clear_input();
    }

    pub(crate) fn filtered_provider_catalog(&self) -> Vec<&'static provider_catalog::ProviderSpec> {
        fuzzy_filter(
            provider_catalog::API_KEY_PROVIDERS,
            self.input.trim(),
            |provider| {
                format!(
                    "{} {} {} {} {}",
                    provider.id,
                    provider.name,
                    provider.env,
                    provider.aliases.join(" "),
                    provider.models.join(" ")
                )
            },
        )
    }

    pub(crate) fn open_provider_menu(&mut self) {
        self.close_config();
        self.selecting_model = false;
        self.provider_menu_open = true;
        self.provider_catalog_choice = 0;
        self.clear_input();
    }

    pub(crate) fn close_provider_menu(&mut self) {
        self.provider_menu_open = false;
        self.provider_catalog_choice = 0;
        self.clear_input();
    }

    pub(crate) fn move_provider_catalog_choice(&mut self, delta: isize) {
        let len = self.filtered_provider_catalog().len();
        if len != 0 {
            self.provider_catalog_choice =
                (self.provider_catalog_choice as isize + delta).rem_euclid(len as isize) as usize;
        }
    }

    pub(crate) fn reset_provider_catalog_choice(&mut self) {
        let len = self.filtered_provider_catalog().len();
        self.provider_catalog_choice = self.provider_catalog_choice.min(len.saturating_sub(1));
    }

    pub(crate) fn selected_provider_catalog(
        &self,
    ) -> Option<&'static provider_catalog::ProviderSpec> {
        self.filtered_provider_catalog()
            .get(self.provider_catalog_choice)
            .copied()
    }

    pub(crate) fn close_config(&mut self) {
        self.config_open = false;
        self.config_choice = 0;
    }

    pub(crate) fn move_config_choice(&mut self, delta: isize) {
        let len = self.config_menu_rows().len();
        if len == 0 {
            return;
        }
        self.config_choice =
            (self.config_choice as isize + delta).rem_euclid(len as isize) as usize;
    }

    /// Run the currently highlighted config-menu entry. Returns `true` when the
    /// menu should stay open (state changed in place), `false` to close it.
    pub(crate) fn activate_config(
        &mut self,
        agent: &Arc<Mutex<Agent>>,
        tx: &tokio::sync::mpsc::UnboundedSender<AppEvent>,
    ) -> bool {
        match self.config_choice {
            0 => {
                self.close_config();
                self.open_model_selector();
                self.refresh_remote_model_choices(tx.clone());
                false
            }
            1 => {
                self.cycle_scope(1, agent);
                true
            }
            2 => {
                self.cycle_effort();
                true
            }
            3 => {
                // OAuth login is interactive (browser flow); drop raw mode here.
                let result = run_login_from_tui(None);
                push_system_message(
                    self,
                    match result {
                        Ok(()) => {
                            "Login complete. Restart tk to load the new provider.".to_string()
                        }
                        Err(error) => format!("Login failed: {error}"),
                    },
                );
                false
            }
            _ => {
                let summary = providers_summary(self);
                push_system_message(self, summary);
                false
            }
        }
    }

    pub(crate) fn take_scrollback(&mut self, width: usize) -> Vec<Line<'static>> {
        use crepuscularity_tui::ratatui::style::Color;

        let count = self
            .messages
            .iter()
            .take_while(|message| !message.is_streaming)
            .count();
        self.messages
            .drain(..count)
            .flat_map(|message| {
                if message.is_tool {
                    let color = if message.role == "error" {
                        Color::Red
                    } else {
                        tool_color(&message.tool_name)
                    };
                    let text = if message.content.is_empty() {
                        message.tool_name
                    } else {
                        format!("{} {}", message.tool_name, message.content)
                    };
                    wrap_scrollback_line("| ", &text, width, color)
                } else if message.role == "user" {
                    message
                        .content
                        .lines()
                        .enumerate()
                        .flat_map(|(index, line)| {
                            wrap_scrollback_line(
                                if index == 0 { "> " } else { "  " },
                                line,
                                width,
                                Color::Cyan,
                            )
                        })
                        .collect::<Vec<_>>()
                } else {
                    if message.role == "error" {
                        message
                            .content
                            .lines()
                            .flat_map(|line| wrap_scrollback_line("", line, width, Color::Red))
                            .collect()
                    } else {
                        markdown::render(&message.content, width)
                    }
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        file_query, load_template, matching_slash_commands, search_files, App, ChatMessage,
        ConfiguredProvider,
    };
    use crate::models::{context_window_for_model, GPT_5_CONTEXT_WINDOW};
    #[cfg(feature = "pi-compat")]
    use crate::pi::{PiEntryType, PiSession};
    use crate::slash::{
        apply_budget_command, budget_summary, clean_search_text, handle_slash_command,
        plan_request, review_request,
    };
    #[cfg(feature = "pi-compat")]
    use crate::tui::restored_chat;
    use crate::tui::{bounded_plan_preview, is_permission_toggle, tool_result_summary};
    use crossterm::event::{KeyCode, KeyModifiers};
    use rx4::permissions::{PlanApprover, PlanDecision, PlanProposal};
    use rx4::provider::OpenAIProvider;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn provider(id: &str) -> ConfiguredProvider {
        ConfiguredProvider {
            id: id.to_string(),
            name: id.to_string(),
            client: Arc::new(OpenAIProvider::with_base_url(
                "http://localhost",
                "test",
                id,
                id,
            )),
        }
    }

    #[test]
    fn plan_and_review_requests_are_read_only_and_specific() {
        let plan = plan_request("add a session browser");
        assert!(plan.contains("add a session browser"));
        assert!(plan.contains("Do not modify the workspace"));
        let review = review_request("ui/tui/src/main.rs");
        assert!(review.contains("ui/tui/src/main.rs"));
        assert!(review.contains("actionable findings"));
        assert!(review.contains("Do not modify the workspace"));
    }

    #[tokio::test]
    async fn plan_approval_preview_round_trips_through_the_tui_channel() {
        let (approver, rx) = rx4::permissions::ChannelPlanApprover::pair();
        let proposal = PlanProposal {
            prompt: "ship the change".to_string(),
            plan: "Inspect, implement, and verify.".to_string(),
            calls: vec![rx4::agent::ToolCall {
                id: "call-1".to_string(),
                name: "bash".to_string(),
                arguments: r#"{"command":"cargo test"}"#.to_string(),
            }],
            turn: 0,
        };
        let worker = tokio::spawn(async move { approver.approve_plan(&proposal).await });
        let mut app = App::new();
        app.plan_rx = Some(rx);
        for _ in 0..10 {
            app.poll_pending_plan_approvals();
            if app.plan_prompt {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(app.plan_prompt);
        assert!(app.plan_rows.iter().any(|line| line.contains("cargo test")));
        app.resolve_plan(true);
        assert_eq!(worker.await.unwrap(), PlanDecision::Approve);
        assert!(!app.plan_prompt);
    }

    #[test]
    fn plan_preview_is_bounded_and_terminal_safe() {
        let proposal = PlanProposal {
            prompt: "inspect".to_string(),
            plan: "\u{1b}[31munsafe\u{1b}[0m".to_string(),
            calls: (0..30)
                .map(|index| rx4::agent::ToolCall {
                    id: format!("call-{index}"),
                    name: "read".to_string(),
                    arguments: "{}".to_string(),
                })
                .collect(),
            turn: 0,
        };
        let rows = bounded_plan_preview(&proposal);
        assert!(rows.len() <= super::PLAN_PREVIEW_MAX_LINES + 1);
        assert!(rows.iter().all(|row| !row.contains('\u{1b}')));
        assert!(rows.last().is_some_and(|row| row.contains("truncated")));
    }

    #[test]
    fn budget_command_exposes_bounded_turn_controls() {
        let mut agent = rx4::agent::Agent::new();
        assert!(budget_summary(&agent).contains("max_turns=50"));
        assert!(apply_budget_command(&mut agent, "cost 1.25").contains("1.2500"));
        assert!(apply_budget_command(&mut agent, "time 90").contains("90s"));
        assert_eq!(
            apply_budget_command(&mut agent, "turns 7"),
            "Budget max_turns set to 7"
        );
        assert_eq!(agent.max_tool_iterations, 7);
        assert!(apply_budget_command(&mut agent, "time 999999").contains("capped"));
        assert_eq!(
            agent
                .budget
                .as_ref()
                .and_then(|budget| budget.max_duration_seconds),
            Some(super::MAX_BUDGET_DURATION_SECONDS)
        );
        assert!(apply_budget_command(&mut agent, "turns 999999").contains("capped"));
        assert_eq!(agent.max_tool_iterations, super::MAX_BUDGET_TURNS);
        assert!(apply_budget_command(&mut agent, "clear").contains("reset to 50"));
        assert_eq!(agent.max_tool_iterations, 50);
    }

    #[test]
    fn file_query_tracks_only_the_active_mention() {
        assert_eq!(file_query("review @src/ma"), Some("src/ma"));
        assert_eq!(file_query("review @src/main.rs next"), None);
        assert_eq!(file_query("plain"), None);
    }

    #[test]
    fn file_search_is_bounded_and_ignore_aware() {
        let paths = search_files("src/", 2);
        assert!(!paths.is_empty());
        assert!(paths.len() <= 2);
        assert!(paths.iter().all(|path| path.contains("src/")));
    }

    #[test]
    fn file_selection_replaces_the_active_mention() {
        let mut app = App::new();
        app.input = "review @src/ma".to_string();
        app.file_suggestions = vec!["src/main.rs".to_string(), "src/markdown.rs".to_string()];
        app.move_file_choice(1);
        app.choose_file();
        assert_eq!(app.input, "review @src/markdown.rs ");
        assert!(app.file_suggestions.is_empty());
    }

    #[test]
    fn slash_suggestions_filter_and_insert_commands() {
        assert_eq!(
            matching_slash_commands("/co"),
            vec![
                "/config".to_string(),
                "/cost".to_string(),
                "/commands".to_string()
            ]
        );
        assert!(matching_slash_commands("/config show").is_empty());

        let mut app = App::new();
        app.input = "/m".to_string();
        app.refresh_slash_suggestions();
        assert!(app.slash_suggestions.contains(&"/mcp".to_string()));
        app.slash_choice = app
            .slash_suggestions
            .iter()
            .position(|command| command == "/model")
            .expect("model suggestion");
        app.choose_slash_command();
        assert_eq!(app.input, "/model ");
        assert!(app.slash_suggestions.is_empty());
    }

    #[test]
    fn provider_catalog_contains_opencode_go_and_every_xiaomi_token_region() {
        assert_eq!(
            super::provider_catalog::find("opencode").unwrap().id,
            "opencode-go"
        );
        for provider in [
            "xiaomi-token-plan-cn",
            "xiaomi-token-plan-ams",
            "xiaomi-token-plan-sgp",
        ] {
            assert!(super::provider_catalog::find(provider).is_some());
        }
        assert_eq!(
            super::provider_catalog::find("claude").unwrap().env,
            "ANTHROPIC_API_KEY"
        );
    }

    #[test]
    fn provider_menu_is_searchable_and_reports_safe_key_setup() {
        let mut app = App::new();
        app.open_provider_menu();
        app.input = "opencode".to_string();
        app.reset_provider_catalog_choice();
        assert_eq!(app.selected_provider_catalog().unwrap().id, "opencode-go");
        let details = crate::providers::api_key_help(app.selected_provider_catalog().unwrap());
        assert!(details.contains("OPENCODE_API_KEY"));
        assert!(details.contains("never written"));
    }

    #[test]
    fn search_results_are_bounded_and_terminal_safe() {
        assert_eq!(clean_search_text("a\tb", 20), "a b");
        assert_eq!(clean_search_text("\u{1b}[31mred", 20), " [31mred");
    }

    #[test]
    fn dismissing_suggestions_keeps_the_input() {
        let mut app = App::new();
        app.input = "/m".to_string();
        app.slash_suggestions = vec!["/model".to_string()];
        app.file_suggestions = vec!["src/main.rs".to_string()];
        app.pending_file_query = Some("src/ma".to_string());

        app.dismiss_suggestions();

        assert_eq!(app.input, "/m");
        assert!(app.slash_suggestions.is_empty());
        assert!(app.file_suggestions.is_empty());
        assert!(app.pending_file_query.is_none());
    }

    #[test]
    fn failed_prompt_is_restored_for_editing() {
        let mut app = App::new();
        app.handle_event(super::AppEvent::PromptFailed {
            prompt: "try this".to_string(),
        });
        assert_eq!(app.input, "try this");
        assert!(app.messages.is_empty());

        app.input = "next prompt".to_string();
        app.handle_event(super::AppEvent::PromptFailed {
            prompt: "try this".to_string(),
        });
        assert_eq!(app.input, "next prompt");
        assert!(app.messages.is_empty());
    }

    #[cfg(feature = "pi-compat")]
    #[test]
    fn continued_session_restores_transcript_and_tool_summary() {
        let mut session = PiSession::new("/project", "grok-4.5");
        session.append_message(rx4::provider::Role::User, "inspect");
        session.append(PiEntryType::Custom {
            extension: "telekinesis.tool_call".to_string(),
            payload: serde_json::json!({
                "id": "call-1",
                "name": "bash",
                "arguments": "{\"command\":\"pwd\"}",
            }),
        });
        session.append(PiEntryType::Custom {
            extension: "telekinesis.tool_result".to_string(),
            payload: serde_json::json!({
                "id": "call-1",
                "content": "/project",
                "is_error": false,
            }),
        });
        session.append_message(rx4::provider::Role::Assistant, "done");

        let messages = restored_chat(&session);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].content, "inspect");
        assert_eq!(messages[1].content, "pwd → /project");
        assert_eq!(messages[2].content, "done");
    }

    #[test]
    fn embedded_template_ignores_stale_home_template() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".telekinesis")).unwrap();
        std::fs::write(
            home.path().join(".telekinesis/shell.crepus"),
            "stale template",
        )
        .unwrap();
        let template = load_template(None).unwrap();
        assert!(template.source().contains("Telekinesis v{version}"));
        assert!(!template.source().contains("stale template"));
    }

    #[test]
    fn explicit_template_override_is_available_for_development() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shell.crepus");
        std::fs::write(&path, "div\n  \"override\"").unwrap();
        assert!(load_template(Some(path.as_os_str())).is_ok());
    }

    #[test]
    fn completed_activity_moves_to_terminal_scrollback() {
        let mut app = App::new();
        app.handle_rx4_event(rx4::agent::Event::ToolCall(rx4::agent::ToolCall {
            id: "read-1".to_string(),
            name: "read".to_string(),
            arguments: r#"{"path":"AGENTS.md"}"#.to_string(),
        }));
        assert!(app.take_scrollback(80).is_empty());
        app.handle_rx4_event(rx4::agent::Event::ToolExecutionEnd(
            rx4::agent::ToolResult {
                id: "read-1".to_string(),
                content: "one\ntwo".to_string(),
                is_error: false,
                error_kind: None,
            },
        ));
        assert_eq!(
            app.take_scrollback(80)
                .into_iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>(),
            ["| read AGENTS.md → 2 lines"]
        );
        assert!(app.messages.is_empty());

        app.messages.push(ChatMessage {
            role: "user".to_string(),
            content: "review this repo".to_string(),
            is_tool: false,
            tool_name: String::new(),
            tool_call_id: String::new(),
            is_streaming: false,
        });
        assert_eq!(
            app.take_scrollback(80)
                .into_iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>(),
            ["> review this repo"]
        );
    }

    #[test]
    fn parallel_tool_results_update_the_matching_activity() {
        let mut app = App::new();
        for (id, name, arguments) in [
            ("read-1", "read", r#"{"path":"AGENTS.md"}"#),
            ("bash-1", "bash", r#"{"command":"pwd"}"#),
        ] {
            app.handle_rx4_event(rx4::agent::Event::ToolCall(rx4::agent::ToolCall {
                id: id.to_string(),
                name: name.to_string(),
                arguments: arguments.to_string(),
            }));
        }
        app.handle_rx4_event(rx4::agent::Event::ToolExecutionEnd(
            rx4::agent::ToolResult {
                id: "read-1".to_string(),
                content: "one\ntwo".to_string(),
                is_error: false,
                error_kind: None,
            },
        ));

        assert!(!app.messages[0].is_streaming);
        assert!(app.messages[0].content.ends_with("2 lines"));
        assert!(app.messages[1].is_streaming);
        assert_eq!(app.messages[1].tool_call_id, "bash-1");
    }

    #[test]
    fn scrollback_wraps_unicode_without_splitting_characters() {
        let mut app = App::new();
        app.messages.push(ChatMessage {
            role: "user".to_string(),
            content: "review — then fix".to_string(),
            is_tool: false,
            tool_name: String::new(),
            tool_call_id: String::new(),
            is_streaming: false,
        });
        assert_eq!(
            app.take_scrollback(10)
                .into_iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>(),
            ["> review", "  — then", "  fix"]
        );
    }

    #[test]
    fn permission_shortcut_and_bash_failures_are_tidy() {
        assert!(is_permission_toggle(
            KeyCode::Char('~'),
            KeyModifiers::SHIFT
        ));
        assert!(is_permission_toggle(
            KeyCode::Char('`'),
            KeyModifiers::SHIFT
        ));
        assert!(!is_permission_toggle(
            KeyCode::Char('`'),
            KeyModifiers::NONE
        ));
        assert_eq!(
            tool_result_summary("bash", "permission denied\n(exit code: -1)", false),
            "failed · exit -1"
        );
        assert_eq!(tool_result_summary("bash", "hello\n", false), "hello");
    }

    #[test]
    fn cancelling_active_turn_denies_prompt_and_stops_streaming() {
        let mut app = App::new();
        let agent = rx4::agent::Agent::new();
        app.cancellation = Some(agent.cancellation_handle());
        app.busy = true;
        app.permission_prompt = true;
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        app.permission_respond = Some(tx);
        app.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: "working".to_string(),
            is_tool: false,
            tool_name: String::new(),
            tool_call_id: String::new(),
            is_streaming: true,
        });

        app.cancel_turn();

        assert_eq!(rx.recv().unwrap(), rx4::permissions::Decision::Deny);
        assert!(!app.permission_prompt);
        assert!(!app.messages[0].is_streaming);
        assert!(app.busy);
        app.handle_event(super::AppEvent::Idle);
        assert!(!app.busy);
    }

    #[test]
    fn cancellation_events_are_not_rendered_as_errors() {
        let mut app = App::new();
        app.cancellation_requested = true;
        app.handle_rx4_event(rx4::agent::Event::Error("request cancelled".to_string()));
        assert!(app.messages.is_empty());

        app.handle_rx4_event(rx4::agent::Event::Error("provider unavailable".to_string()));
        assert_eq!(app.messages.len(), 1);
    }

    #[test]
    fn model_selector_and_effort_cycle_update_state() {
        let mut app = App::new();
        app.providers = vec![provider("openai")];
        app.open_model_selector();
        assert!(app.model_choice.is_some());
        assert!(!app.model_choices.is_empty());
        assert!(app
            .filtered_models()
            .iter()
            .all(|model| model.provider == "openai"));
        app.move_model_choice(1);
        app.choose_model();
        assert!(app.model_choice.is_none());
        assert!(app.model.starts_with("gpt-"));

        app.cycle_effort();
        assert_eq!(app.effort, "xhigh");
        app.cycle_effort();
        assert_eq!(app.effort, "low");
    }

    #[test]
    fn codex_and_gpt5_models_use_their_context_window() {
        let mut app = App::new();
        app.providers = vec![provider("openai-codex")];
        app.open_model_selector();
        // The codex catalog matches pi's openai-codex.json exactly: the four
        // rs_ai_oauth models plus gpt-5.6-luna/sol/terra.
        for model in rs_ai_oauth::codex::CHATGPT_CODEX_MODELS {
            assert!(app.model_choices.iter().any(|choice| choice.id == *model));
            if model.starts_with("gpt-5.5") {
                assert_eq!(context_window_for_model(model), GPT_5_CONTEXT_WINDOW);
            }
        }
        for model in super::PI_CODEX_GPT56 {
            assert!(
                app.model_choices.iter().any(|choice| choice.id == model),
                "missing codex model {model}"
            );
            assert_eq!(context_window_for_model(model), GPT_5_CONTEXT_WINDOW);
        }
        // pi parity: models that belong to the openai API catalog, not codex.
        for model in ["gpt-5.5-pro", "gpt-5.4-pro", "gpt-5.4-nano"] {
            assert!(
                !app.model_choices
                    .iter()
                    .any(|choice| choice.id == model && choice.provider == "openai-codex"),
                "{model} is not in pi's codex catalog"
            );
        }

        app.context_tokens = 525_000;
        app.set_model("gpt-5.5".to_string());
        assert_eq!(app.context_window, GPT_5_CONTEXT_WINDOW);
        assert_eq!(app.context_pct, 50);
    }

    #[test]
    fn openai_provider_lists_pi_gpt5_catalog() {
        let mut app = App::new();
        app.providers = vec![provider("openai")];
        app.open_model_selector();
        for model in super::PI_OPENAI_GPT5 {
            assert!(
                app.model_choices.iter().any(|choice| choice.id == model),
                "missing openai model {model}"
            );
        }
        // Context windows follow pi's catalog for models rx4 lacks.
        assert_eq!(
            context_window_for_model("gpt-5.5-pro"),
            GPT_5_CONTEXT_WINDOW
        );
        assert_eq!(
            context_window_for_model("gpt-5.4-pro"),
            GPT_5_CONTEXT_WINDOW
        );
        assert_eq!(context_window_for_model("gpt-5-mini"), 400_000);
        assert_eq!(context_window_for_model("gpt-5.3-codex-spark"), 128_000);
    }

    #[test]
    fn model_search_collapses_providers() {
        let mut app = App::new();
        app.providers = vec![provider("openai"), provider("openai-codex")];
        app.open_model_selector();
        // The provider rail sits on the first configured provider ("openai"), so
        // without a query only that provider's models show.
        assert!(app
            .filtered_models()
            .iter()
            .all(|model| model.provider == "openai"));
        // A query searches the whole catalog across every configured provider.
        app.input = "gpt-5.4".to_string();
        app.reset_model_choice();
        let ids: Vec<&str> = app
            .filtered_models()
            .iter()
            .map(|model| model.id.as_str())
            .collect();
        assert!(
            ids.contains(&"gpt-5.4"),
            "search should cross providers, got {ids:?}"
        );
        assert!(ids.iter().all(|id| id.contains("gpt-5.4")));
    }

    #[test]
    fn config_menu_and_thinking_render_without_template_errors() {
        use crepuscularity_tui::ratatui::backend::TestBackend;
        use crepuscularity_tui::ratatui::Terminal;

        let mut template = load_template(None).unwrap();
        let mut app = App::new();
        app.config_open = true;
        app.config_choice = 1;
        app.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: String::new(),
            is_tool: false,
            tool_name: String::new(),
            tool_call_id: String::new(),
            is_streaming: true,
        });
        app.update_template(&mut template);
        let mut terminal = Terminal::new(TestBackend::new(200, 20)).unwrap();
        terminal
            .draw(|frame| template.draw(frame, frame.area()).unwrap())
            .unwrap();
        let output =
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .fold(String::new(), |mut out, cell| {
                    out.push_str(cell.symbol());
                    out
                });
        assert!(output.contains("config ·"));
        assert!(output.contains("scope ·"));
        assert!(output.contains("thinking"));
    }

    #[test]
    fn cycle_scope_wraps_through_all_scopes() {
        let mut app = App::new();
        let agent = rx4::agent::Agent::new();
        let agent = Arc::new(Mutex::new(agent));
        app.agent_mode = "coding".to_string();
        app.cycle_scope(1, &agent);
        assert_eq!(app.agent_mode, "research");
        app.cycle_scope(1, &agent);
        assert_eq!(app.agent_mode, "plan");
        if cfg!(feature = "computer-use") {
            app.cycle_scope(2, &agent);
            assert_eq!(app.agent_mode, "computer_use");
            app.cycle_scope(1, &agent);
            assert_eq!(app.agent_mode, "coding");
            app.cycle_scope(-1, &agent);
            assert_eq!(app.agent_mode, "computer_use");
        } else {
            app.cycle_scope(1, &agent);
            assert_eq!(app.agent_mode, "ask");
            app.cycle_scope(1, &agent);
            assert_eq!(app.agent_mode, "coding");
            app.cycle_scope(-1, &agent);
            assert_eq!(app.agent_mode, "ask");
        }
    }

    #[test]
    fn config_menu_opens_and_activates() {
        let mut app = App::new();
        app.config_open = false;
        app.open_config();
        assert!(app.config_open);
        assert_eq!(app.config_choice, 0);
        let agent = rx4::agent::Agent::new();
        let agent = Arc::new(Mutex::new(agent));
        let (_tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        // Choice 1 cycles scope in place and keeps the menu open.
        app.config_choice = 1;
        assert!(app.activate_config(&agent, &_tx));
        assert!(app.config_open);
        // Choice 4 shows the summary; a `false` return tells the caller to close.
        app.config_choice = 4;
        assert!(!app.activate_config(&agent, &_tx));
        app.close_config();
        assert!(!app.config_open);
        assert!(app.messages.iter().any(|m| m.role == "system"));
    }

    #[test]
    fn embedded_template_renders_compact_header_and_prompt() {
        use crepuscularity_tui::ratatui::backend::TestBackend;
        use crepuscularity_tui::ratatui::Terminal;

        let mut template = load_template(None).unwrap();
        App::new().update_template(&mut template);
        let mut terminal = Terminal::new(TestBackend::new(200, 9)).unwrap();
        terminal
            .draw(|frame| template.draw(frame, frame.area()).unwrap())
            .unwrap();
        let output =
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .fold(String::new(), |mut output, cell| {
                    output.push_str(cell.symbol());
                    output
                });
        assert!(output.contains("$0.000"));
        assert!(output.contains("no-model · high"));
        assert!(output.contains("> "));
    }

    #[test]
    fn paste_collapses_large_content_and_keeps_short_multiline_content() {
        let mut app = App::new();
        app.insert_newline();
        assert_eq!(app.input, "\n");
        app.clear_input();
        app.paste("first\r\nsecond");
        assert_eq!(app.input, "first\nsecond");

        let large = (1..=11)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        app.clear_input();
        app.paste(&large);
        assert_eq!(app.input, "[paste #1]");
        assert_eq!(app.expanded_input(), large);
    }

    #[test]
    fn input_cursor_edits_in_the_middle_of_the_draft() {
        let mut app = App::new();
        app.input = "fix the bug".to_string();
        app.cursor_to_end();
        app.move_cursor(-3);
        app.insert_at_cursor("real ");
        assert_eq!(app.input, "fix the real bug");

        // Backspace removes the char before the cursor.
        app.input = "fix the bug".to_string();
        app.cursor_to_end();
        app.move_cursor(-3);
        app.delete_back_at_cursor();
        assert_eq!(app.input, "fix thebug");

        // Delete removes the char after the cursor.
        app.input = "fix the bug".to_string();
        app.cursor_to_end();
        app.move_cursor(-3);
        app.delete_forward_at_cursor();
        assert_eq!(app.input, "fix the ug");

        // Ctrl+U / Ctrl+K style deletes.
        app.input = "fix the bug".to_string();
        app.cursor_to_end();
        app.move_cursor(-3);
        app.delete_to_start();
        assert_eq!(app.input, "bug");
        app.delete_to_end();
        assert_eq!(app.input, "");
    }

    #[test]
    fn input_word_navigation_and_delete_word() {
        let mut app = App::new();
        app.input = "one two three".to_string();
        app.cursor_to_start();
        app.move_word(1);
        assert_eq!(app.cursor, 4);
        app.move_word(1);
        assert_eq!(app.cursor, 8);
        app.move_word(-1);
        assert_eq!(app.cursor, 4);

        app.input = "one two three".to_string();
        app.cursor_to_end();
        app.delete_word_back();
        assert_eq!(app.input, "one two ");

        // From the middle of a word, delete back to the word start (emacs-style).
        app.input = "one two three".to_string();
        app.cursor_to_end();
        app.move_cursor(-1);
        app.delete_word_back();
        assert_eq!(app.input, "one two e");
    }

    #[test]
    fn fuzzy_matching_ranks_subsequence_and_swap_matches() {
        use super::fuzzy_match;
        assert!(fuzzy_match("gpt55", "gpt-5.5").is_some(), "swap fallback");
        assert!(fuzzy_match("5.5", "gpt-5.5").is_some());
        assert!(fuzzy_match("openai", "openai gpt-5.5").is_some());
        assert!(fuzzy_match("zzz", "gpt-5.5").is_none());
        // Exact match ranks better than a gap-heavy subsequence.
        let exact = fuzzy_match("gpt-5.5", "gpt-5.5").unwrap();
        let fuzzy = fuzzy_match("gpt55", "gpt-5.5").unwrap();
        assert!(exact < fuzzy);
        // Consecutive matches rank better than spread-out ones.
        let consecutive = fuzzy_match("gpt", "gpt-5.5").unwrap();
        let spread = fuzzy_match("g55", "gpt-5.5").unwrap();
        assert!(consecutive < spread);
    }

    #[test]
    fn model_search_uses_fuzzy_provider_text() {
        let mut app = App::new();
        app.providers = vec![provider("openai-codex")];
        app.open_model_selector();
        // "codex 55" matches via provider + swapped digits, not just the id.
        app.input = "codex 55".to_string();
        app.reset_model_choice();
        let ids: Vec<&str> = app
            .filtered_models()
            .iter()
            .map(|model| model.id.as_str())
            .collect();
        assert!(
            ids.contains(&"gpt-5.5"),
            "fuzzy provider search, got {ids:?}"
        );
    }

    #[test]
    fn undo_restores_previous_edits_and_cursor() {
        let mut app = App::new();
        app.insert_at_cursor("hello ");
        app.insert_at_cursor("world");
        app.undo();
        assert_eq!(app.input, "hello ");
        app.undo();
        assert_eq!(app.input, "");

        // Undo also restores the cursor position.
        app.input = "abc".to_string();
        app.cursor = 1;
        app.delete_forward_at_cursor();
        assert_eq!(app.input, "ac");
        app.undo();
        assert_eq!(app.input, "abc");
        assert_eq!(app.cursor, 1);
    }

    #[test]
    fn slash_commands_have_descriptions() {
        use super::slash_description;
        assert!(slash_description("/model").contains("model"));
        assert!(slash_description("/clear").contains("clear"));
        assert_eq!(slash_description("/unknown"), "");
    }

    #[test]
    fn model_argument_completion_lists_fuzzy_models() {
        let mut app = App::new();
        app.providers = vec![provider("openai-codex")];
        app.refresh_model_choices();
        app.input = "/model 5.4".to_string();
        app.refresh_slash_suggestions();
        assert!(app
            .slash_suggestions
            .iter()
            .any(|suggestion| suggestion == "/model gpt-5.4"));
        // Descriptions resolve to the model's provider (pi-style).
        let desc = app.slash_row_description("/model gpt-5.4");
        assert_eq!(desc, "openai-codex");
    }

    #[test]
    fn enter_completes_and_applies_model_argument() {
        let mut app = App::new();
        let agent = rx4::agent::Agent::new();
        let agent = Arc::new(Mutex::new(agent));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        app.providers = vec![provider("openai-codex")];
        app.refresh_model_choices();
        // Type "/model 5.4" → suggestions appear → Enter completes + applies.
        app.input = "/model 5.4".to_string();
        app.refresh_slash_suggestions();
        assert!(!app.slash_suggestions.is_empty());
        app.choose_slash_command();
        assert_eq!(app.input, "/model gpt-5.4 ");
        let text = app.input.trim().to_string();
        handle_slash_command(&mut app, &text, &agent, &tx);
        assert_eq!(app.model, "gpt-5.4");
    }

    #[test]
    fn commands_alias_lists_and_describes_commands() {
        let mut app = App::new();
        let agent = rx4::agent::Agent::new();
        let agent = Arc::new(Mutex::new(agent));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        handle_slash_command(&mut app, "/commands", &agent, &tx);
        assert!(app
            .messages
            .iter()
            .any(|message| message.content.contains("Commands:")));

        app.messages.clear();
        handle_slash_command(&mut app, "/commands model", &agent, &tx);
        let last = app.messages.last().expect("usage message");
        assert!(last.content.contains("/model"));
        assert!(last.content.contains("pick or set the model"));
    }

    #[test]
    fn mcp_and_search_slash_commands_report_feature_gates() {
        let mut app = App::new();
        let agent = Arc::new(Mutex::new(rx4::agent::Agent::new()));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        handle_slash_command(&mut app, "/mcp", &agent, &tx);
        let mcp = app.messages.last().expect("mcp message").content.clone();
        handle_slash_command(&mut app, "/search", &agent, &tx);
        let search = app.messages.last().expect("search message").content.clone();
        if cfg!(feature = "mcp") {
            assert!(mcp.contains("MCP") || mcp.contains("Config:"));
        } else {
            assert!(mcp.contains("--features mcp"));
        }
        if cfg!(feature = "search") {
            assert!(search.contains("web_search"));
        } else {
            assert!(search.contains("--features search"));
        }
    }

    #[test]
    fn file_search_debounces_until_deadline() {
        let mut app = App::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        app.input = "read @src/ma".to_string();
        app.refresh_file_suggestions();
        assert!(app.pending_file_query.is_some());
        assert!(app.file_search_deadline.is_some());
        // Not yet due → nothing spawns and the query stays pending.
        app.maybe_run_file_search(tx.clone());
        assert!(app.pending_file_query.is_some());
        // Same query → not re-armed.
        app.refresh_file_suggestions();
        assert!(app.pending_file_query.is_some());
        // Dropping the mention cancels the pending search.
        app.input = "read".to_string();
        app.refresh_file_suggestions();
        assert!(app.pending_file_query.is_none());
        assert!(app.file_search_deadline.is_none());
    }

    #[test]
    fn embedded_template_keeps_status_rows_adjacent_and_flush() {
        use crepuscularity_tui::ratatui::backend::TestBackend;
        use crepuscularity_tui::ratatui::Terminal;

        let mut template = load_template(None).unwrap();
        App::new().update_template(&mut template);
        let mut terminal = Terminal::new(TestBackend::new(200, 9)).unwrap();
        terminal
            .draw(|frame| template.draw(frame, frame.area()).unwrap())
            .unwrap();
        let buffer = terminal.backend().buffer();
        let width = buffer.area.width as usize;
        let rows = buffer
            .content()
            .chunks(width)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();
        let cost_row = rows.iter().position(|row| row.contains("$0.000")).unwrap();
        let model_row = rows
            .iter()
            .position(|row| row.contains("no-model · high"))
            .unwrap();
        assert_eq!(cost_row + 1, model_row);
        assert_eq!(rows[cost_row].find('$'), Some(0));
    }
}
