use std::io::{stdout, Write};
use std::path::PathBuf;
use std::sync::Arc;

use crepuscularity_tui::ratatui::backend::CrosstermBackend;
use crepuscularity_tui::ratatui::text::Line;
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use rx4::agent::Event as Rx4Event;
use rx4::permissions::PlanProposal;
use rx4::provider::Role;
use tokio::sync::Mutex;

use crate::app::{
    load_template, App, AppEvent, ChatMessage, PLAN_PREVIEW_LINE_LIMIT, PLAN_PREVIEW_MAX_LINES,
};
use crate::channel_approver::ChannelApprover;
use crate::host::{self, apply_scope, load_prefs, parse_host_scope};
use crate::models::initial_model_registry;
#[cfg(feature = "pi-compat")]
use crate::pi::{self, PiEntryType, PiSession};
use crate::providers::{
    api_key_help, choose_provider, push_system_message, run_login, setup_providers,
};
use crate::slash::{clean_search_text, handle_slash_command};
use crate::tools::discover_mcp_tools;

#[cfg(feature = "pi-compat")]
pub(crate) fn newest_session(dir: &std::path::Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
        .max_by_key(|entry| {
            entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
        })
        .map(|entry| entry.path())
}

/// Newest-first JSONL session files for this project, capped for display.
#[cfg(feature = "pi-compat")]
pub(crate) fn session_files() -> Vec<PathBuf> {
    let dir = pi::pi_sessions_dir(&std::env::current_dir().unwrap_or_default());
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
                .collect()
        })
        .unwrap_or_default();
    files.sort_by_key(|path| {
        std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::UNIX_EPOCH)
    });
    files.reverse();
    files.truncate(20);
    files
}

#[cfg(feature = "pi-compat")]
pub(crate) fn restored_chat(session: &PiSession) -> Vec<ChatMessage> {
    let mut messages: Vec<ChatMessage> = session
        .entries
        .iter()
        .filter_map(|entry| match &entry.entry_type {
            PiEntryType::Message { role, content, .. } => Some(ChatMessage {
                role: match role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "tool",
                    Role::System => "system",
                }
                .to_string(),
                content: content.clone(),
                is_tool: *role == Role::Tool,
                tool_name: if *role == Role::Tool {
                    "tool".to_string()
                } else {
                    String::new()
                },
                tool_call_id: String::new(),
                is_streaming: false,
            }),
            PiEntryType::Custom { extension, payload } if extension == "telekinesis.tool_call" => {
                Some(ChatMessage {
                    role: "tool".to_string(),
                    content: tool_detail(
                        payload["name"].as_str().unwrap_or("tool"),
                        payload["arguments"].as_str().unwrap_or_default(),
                    ),
                    is_tool: true,
                    tool_name: payload["name"].as_str().unwrap_or("tool").to_string(),
                    tool_call_id: payload["id"].as_str().unwrap_or_default().to_string(),
                    is_streaming: false,
                })
            }
            PiEntryType::Compaction { summary, .. } => Some(ChatMessage {
                role: "tool".to_string(),
                content: summary.clone(),
                is_tool: true,
                tool_name: "compacted context".to_string(),
                tool_call_id: String::new(),
                is_streaming: false,
            }),
            _ => None,
        })
        .collect();
    for entry in &session.entries {
        let PiEntryType::Custom { extension, payload } = &entry.entry_type else {
            continue;
        };
        if extension != "telekinesis.tool_result" {
            continue;
        }
        let id = payload["id"].as_str().unwrap_or_default();
        if let Some(message) = messages
            .iter_mut()
            .rev()
            .find(|message| message.tool_call_id == id)
        {
            let detail = std::mem::take(&mut message.content);
            let summary = tool_result_summary(
                &message.tool_name,
                payload["content"].as_str().unwrap_or_default(),
                payload["is_error"].as_bool().unwrap_or(false),
            );
            message.content = if detail.is_empty() {
                summary
            } else {
                format!("{detail} → {summary}")
            };
        }
    }
    messages
}

pub(crate) fn run_tui(continue_session: bool) -> anyhow::Result<()> {
    let mut tpl = load_template(std::env::var_os("TELEKINESIS_TEMPLATE").as_deref())?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let discover = rt.spawn(discover_mcp_tools());
    let mut providers = setup_providers(&rt);
    if providers.is_empty() {
        run_login(Some(choose_provider()?))?;
        providers = setup_providers(&rt);
    }
    let (mcp_specs, mcp_errors) = rt
        .block_on(discover)
        .map_err(|error| anyhow::anyhow!("MCP discover failed: {error}"))?;
    #[cfg(feature = "pi-compat")]
    let session_dir = pi::pi_sessions_dir(&std::env::current_dir()?);
    #[cfg(feature = "pi-compat")]
    let loaded_session = continue_session
        .then(|| newest_session(&session_dir))
        .flatten()
        .map(|path| PiSession::load_jsonl(&path))
        .transpose()?;
    #[cfg(feature = "pi-compat")]
    let resumed_model = loaded_session.as_ref().map(|session| {
        session
            .entries
            .iter()
            .rev()
            .find_map(|entry| match &entry.entry_type {
                PiEntryType::ModelChange { to, .. } => Some(to.clone()),
                _ => None,
            })
            .unwrap_or_else(|| session.header.model.clone())
    });
    #[cfg(feature = "pi-compat")]
    let resumed_effort = loaded_session
        .as_ref()
        .and_then(|session| {
            session
                .entries
                .iter()
                .rev()
                .find_map(|entry| match &entry.entry_type {
                    PiEntryType::ThinkingLevelChange { level } => Some(level.clone()),
                    _ => None,
                })
        })
        .unwrap_or_else(|| "high".to_string());
    #[cfg(not(feature = "pi-compat"))]
    let resumed_model: Option<String> = None;
    #[cfg(not(feature = "pi-compat"))]
    let resumed_effort = "high".to_string();
    // Persisted preferences are the source of truth for model/scope/effort;
    // they win over the per-session resume so changes stick across restarts.
    let prefs = load_prefs();
    let preferred_model = prefs.model.clone().or(resumed_model);
    let effort = prefs.effort.clone().unwrap_or(resumed_effort.clone());
    let initial_registry = initial_model_registry(&providers);
    let preferred_provider = preferred_model
        .as_deref()
        .and_then(|model| {
            initial_registry.get(model).and_then(|entry| {
                providers
                    .iter()
                    .position(|item| item.0.id == entry.provider)
            })
        })
        .unwrap_or(0);
    let (provider, model) = if let Some(selected) = providers.get(preferred_provider).cloned() {
        (selected.0.client, preferred_model.unwrap_or(selected.1))
    } else {
        anyhow::bail!("Login completed without a usable token");
    };

    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();

    let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (mut agent, subagent_manager) = host::build_agent(
        Some(provider.clone()),
        &model,
        &effort,
        workspace,
        initial_registry.clone(),
        &mcp_specs,
    );
    let restored_scope = prefs
        .scope
        .as_deref()
        .and_then(|scope| parse_host_scope(scope).ok());
    if let Some(scope) = restored_scope {
        apply_scope(&mut agent, scope);
    }
    #[cfg(feature = "pi-compat")]
    if let Some(session) = &loaded_session {
        *agent.messages.write() = session.messages();
    }
    let (approver, approval_rx) = ChannelApprover::pair();
    let approval_mode = approver.mode();
    agent.set_approver(Arc::new(approver));

    // rx4 owns the plan gate and the wait; the TUI only presents the bounded
    // proposal and returns the user's decision. Set TK_PLAN_APPROVAL=off for
    // non-interactive compatibility, or =bypass for an explicit yolo mode.
    let plan_rx = match std::env::var("TK_PLAN_APPROVAL").as_deref() {
        Ok("off") | Ok("disabled") => None,
        Ok("bypass") | Ok("allow") => {
            agent.set_plan_approver(Arc::new(rx4::permissions::AlwaysApprovePlan));
            None
        }
        _ => {
            let (plan_approver, plan_rx) = rx4::permissions::ChannelPlanApprover::pair();
            agent.set_plan_approver(Arc::new(plan_approver));
            Some(plan_rx)
        }
    };

    let event_tx_clone = event_tx.clone();
    agent.subscribe(move |event: &Rx4Event| {
        let _ = event_tx_clone.send(AppEvent::Rx4(event.clone()));
    });

    let cancellation = agent.cancellation_handle();
    let agent = Arc::new(Mutex::new(agent));
    #[cfg(feature = "mcp")]
    let mcp_names: Vec<String> = mcp_specs
        .iter()
        .map(|spec| spec.full_name.clone())
        .collect();
    #[cfg(not(feature = "mcp"))]
    let mcp_names: Vec<String> = Vec::new();
    for error in mcp_errors {
        let _ = event_tx.send(AppEvent::Error(error));
    }

    let mut app = App::new();
    app.set_model(model);
    app.effort = effort;
    app.agent_mode = restored_scope
        .map(|scope| scope.name().to_string())
        .unwrap_or_else(|| "coding".to_string());
    app.mcp_tools = mcp_names;
    app.mcp_connecting = false;
    #[cfg(feature = "pi-compat")]
    {
        app.messages = loaded_session
            .as_ref()
            .map(restored_chat)
            .unwrap_or_default();
        app.session = Some((
            loaded_session.unwrap_or_else(|| {
                PiSession::new(
                    std::env::current_dir()
                        .unwrap_or_else(|_| PathBuf::from("."))
                        .to_string_lossy(),
                    app.model.clone(),
                )
            }),
            session_dir,
        ));
        app.persist()?;
    }
    app.providers = providers
        .into_iter()
        .map(|(provider, _)| provider)
        .collect();
    app.model_registry = initial_registry;
    app.refresh_model_choices();
    app.agent = Some(agent.clone());
    app.cancellation = Some(cancellation);
    app.event_rx = Some(event_rx);
    app.approval_rx = Some(approval_rx);
    app.plan_rx = plan_rx;
    app.approval_mode = Some(approval_mode);
    app.subagent_manager = Some(subagent_manager);
    app.prefs_enabled = true;

    let _rt_guard = rt.enter();

    enable_raw_mode()?;
    let mut stdout = stdout();
    crossterm::execute!(stdout, EnableBracketedPaste)?;
    stdout.flush()?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = crepuscularity_tui::ratatui::Terminal::with_options(
        backend,
        crepuscularity_tui::ratatui::TerminalOptions {
            viewport: crepuscularity_tui::ratatui::Viewport::Inline(9),
        },
    )?;

    loop {
        let mut pending = Vec::new();
        if let Some(rx) = app.event_rx.as_mut() {
            while let Ok(event) = rx.try_recv() {
                pending.push(event);
            }
        }
        for event in pending {
            app.handle_event(event);
        }
        app.poll_pending_plan_approvals();
        app.poll_pending_approvals();
        app.refresh_branch();
        app.maybe_run_file_search(event_tx.clone());

        let width = terminal.size()?.width;
        let scrollback = app.take_scrollback(width as usize);
        if !scrollback.is_empty() {
            terminal.insert_before(scrollback.len() as u16, |buffer| {
                for (index, line) in scrollback.iter().enumerate() {
                    buffer.set_line(0, index as u16, line, width);
                }
            })?;
        }

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

        if crossterm::event::poll(std::time::Duration::from_millis(100))? {
            match crossterm::event::read()? {
                Event::Paste(pasted) => {
                    app.paste(&pasted);
                    if app.selecting_model {
                        app.reset_model_choice();
                    }
                }
                Event::Key(key) => {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    if app.plan_prompt {
                        match key.code {
                            KeyCode::Char('y') | KeyCode::Char('Y') => {
                                app.resolve_plan(true);
                            }
                            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                                app.resolve_plan(false);
                                if key.code == KeyCode::Esc {
                                    app.cancel_turn();
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }
                    if is_permission_toggle(key.code, key.modifiers) {
                        app.toggle_permission_mode();
                        continue;
                    }
                    if app.permission_prompt {
                        match key.code {
                            KeyCode::Char('y') | KeyCode::Char('Y') => {
                                app.resolve_permission(true);
                                continue;
                            }
                            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                                app.resolve_permission(false);
                                if key.code == KeyCode::Esc {
                                    app.cancel_turn();
                                }
                                continue;
                            }
                            _ => continue,
                        }
                    }
                    match (key.code, key.modifiers) {
                        (_code, _mods) if app.config_open => {
                            match key.code {
                                KeyCode::Enter => {
                                    if !app.activate_config(&agent, &event_tx) {
                                        app.close_config();
                                    }
                                }
                                KeyCode::Esc => {
                                    app.close_config();
                                }
                                KeyCode::Up => {
                                    app.move_config_choice(-1);
                                }
                                KeyCode::Down => {
                                    app.move_config_choice(1);
                                }
                                _ => {}
                            }
                            continue;
                        }
                        (_code, _mods) if app.provider_menu_open => {
                            match key.code {
                                KeyCode::Enter => {
                                    if let Some(provider) = app.selected_provider_catalog() {
                                        let detail = api_key_help(provider);
                                        app.close_provider_menu();
                                        push_system_message(&mut app, detail);
                                    }
                                }
                                KeyCode::Esc => {
                                    app.close_provider_menu();
                                }
                                KeyCode::Up => app.move_provider_catalog_choice(-1),
                                KeyCode::Down => app.move_provider_catalog_choice(1),
                                KeyCode::Backspace => {
                                    app.delete_back_at_cursor();
                                    app.reset_provider_catalog_choice();
                                }
                                KeyCode::Delete => {
                                    app.delete_forward_at_cursor();
                                    app.reset_provider_catalog_choice();
                                }
                                KeyCode::Char(character) => {
                                    app.insert_at_cursor(&character.to_string());
                                    app.reset_provider_catalog_choice();
                                }
                                _ => {}
                            }
                            continue;
                        }
                        (KeyCode::Enter, KeyModifiers::SHIFT) => {
                            app.insert_newline();
                        }
                        (KeyCode::Tab, _) if !app.slash_suggestions.is_empty() => {
                            app.choose_slash_command();
                        }
                        (KeyCode::Tab, _) if !app.file_suggestions.is_empty() => {
                            app.choose_file();
                        }
                        (KeyCode::Enter, _) => {
                            if app.selecting_model {
                                app.choose_model();
                                continue;
                            }
                            if app.busy {
                                continue;
                            }
                            // Complete the highlighted suggestion first so
                            // "/model deep" + Enter applies it directly
                            // (pi/Claude-style: type → arrows → enter).
                            if !app.slash_suggestions.is_empty() {
                                app.choose_slash_command();
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
                                app.cancel_turn();
                            } else if !app.input.is_empty() {
                                // pi convention: Ctrl+C clears the draft first,
                                // a second press (empty input) exits.
                                app.clear_input();
                            } else {
                                break;
                            }
                        }
                        (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                            if app.input.is_empty() {
                                break;
                            }
                        }
                        (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                            app.cursor_to_start();
                        }
                        (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                            app.cursor_to_end();
                        }
                        (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                            app.delete_to_end();
                        }
                        (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                            app.delete_to_start();
                        }
                        (KeyCode::Char('w'), KeyModifiers::CONTROL) => {
                            app.delete_word_back();
                        }
                        (KeyCode::Char('z'), KeyModifiers::CONTROL) => {
                            app.undo();
                            app.refresh_slash_suggestions();
                            app.refresh_file_suggestions();
                        }
                        (KeyCode::Char('l'), KeyModifiers::CONTROL) => {
                            let _ = terminal.clear();
                        }
                        (KeyCode::Char('b'), KeyModifiers::CONTROL) => {
                            app.show_header = !app.show_header;
                        }
                        (KeyCode::F(1), _) => {
                            handle_slash_command(&mut app, "/help", &agent, &event_tx);
                        }
                        (KeyCode::BackTab, _) => {
                            app.cycle_effort();
                        }
                        (KeyCode::Esc, _) if app.selecting_model => {
                            app.model_choice = None;
                            app.selecting_model = false;
                            app.clear_input();
                        }
                        (KeyCode::Esc, _) if app.busy => {
                            app.cancel_turn();
                        }
                        (KeyCode::Esc, _)
                            if !app.slash_suggestions.is_empty()
                                || !app.file_suggestions.is_empty() =>
                        {
                            app.dismiss_suggestions();
                        }
                        // Idle Esc with a draft clears it (menu Esc handled above).
                        (KeyCode::Esc, _) if !app.input.is_empty() => {
                            app.clear_input();
                        }
                        (KeyCode::Left, modifiers)
                            if modifiers.contains(KeyModifiers::ALT)
                                && modifiers.contains(KeyModifiers::SHIFT)
                                && !app.selecting_model =>
                        {
                            app.cycle_scope(-1, &agent);
                        }
                        (KeyCode::Right, modifiers)
                            if modifiers.contains(KeyModifiers::ALT)
                                && modifiers.contains(KeyModifiers::SHIFT)
                                && !app.selecting_model =>
                        {
                            app.cycle_scope(1, &agent);
                        }
                        (KeyCode::Left, _) if app.selecting_model => {
                            app.move_provider_choice(-1);
                        }
                        (KeyCode::Right, _) if app.selecting_model => {
                            app.move_provider_choice(1);
                        }
                        (KeyCode::Left, modifiers)
                            if modifiers.contains(KeyModifiers::CONTROL)
                                || modifiers.contains(KeyModifiers::ALT) =>
                        {
                            app.move_word(-1);
                        }
                        (KeyCode::Right, modifiers)
                            if modifiers.contains(KeyModifiers::CONTROL)
                                || modifiers.contains(KeyModifiers::ALT) =>
                        {
                            app.move_word(1);
                        }
                        (KeyCode::Left, _) => {
                            app.move_cursor(-1);
                        }
                        (KeyCode::Right, _) => {
                            app.move_cursor(1);
                        }
                        (KeyCode::Up, _) => {
                            if app.selecting_model {
                                app.move_model_choice(-1);
                                continue;
                            }
                            if !app.slash_suggestions.is_empty() {
                                app.move_slash_choice(-1);
                                continue;
                            }
                            if !app.file_suggestions.is_empty() {
                                app.move_file_choice(-1);
                                continue;
                            }
                            if app.history_index.is_none() && !app.input_history.is_empty() {
                                app.history_draft = app.input.clone();
                                app.history_index = Some(0);
                                app.input = app.history_get();
                                app.cursor_to_end();
                            } else if let Some(idx) = app.history_index {
                                if idx + 1 < app.input_history.len() {
                                    app.history_index = Some(idx + 1);
                                    app.input = app.history_get();
                                    app.cursor_to_end();
                                }
                            }
                        }
                        (KeyCode::Down, _) => {
                            if app.selecting_model {
                                app.move_model_choice(1);
                                continue;
                            }
                            if !app.slash_suggestions.is_empty() {
                                app.move_slash_choice(1);
                                continue;
                            }
                            if !app.file_suggestions.is_empty() {
                                app.move_file_choice(1);
                                continue;
                            }
                            if let Some(idx) = app.history_index {
                                if idx == 0 {
                                    app.history_index = None;
                                    app.input = app.history_draft.clone();
                                    app.cursor_to_end();
                                } else {
                                    app.history_index = Some(idx - 1);
                                    app.input = app.history_get();
                                    app.cursor_to_end();
                                }
                            }
                        }
                        (KeyCode::Backspace, modifiers) => {
                            if modifiers.contains(KeyModifiers::CONTROL)
                                || modifiers.contains(KeyModifiers::ALT)
                            {
                                app.delete_word_back();
                            } else {
                                app.delete_back_at_cursor();
                            }
                            if app.selecting_model {
                                app.reset_model_choice();
                            } else {
                                app.refresh_slash_suggestions();
                                app.refresh_file_suggestions();
                            }
                        }
                        (KeyCode::Delete, _) => {
                            app.delete_forward_at_cursor();
                        }
                        (KeyCode::PageUp, _) => {
                            app.auto_scroll = false;
                        }
                        (KeyCode::PageDown, _) => {
                            app.auto_scroll = true;
                        }
                        (KeyCode::Home, KeyModifiers::CONTROL) => {
                            app.auto_scroll = false;
                        }
                        (KeyCode::End, KeyModifiers::CONTROL) => {
                            app.auto_scroll = true;
                        }
                        (KeyCode::Home, _) => {
                            app.cursor_to_start();
                        }
                        (KeyCode::End, _) => {
                            app.cursor_to_end();
                        }
                        (KeyCode::Char(c), _) => {
                            app.insert_at_cursor(&c.to_string());
                            if app.selecting_model {
                                app.reset_model_choice();
                            } else {
                                app.refresh_slash_suggestions();
                                app.refresh_file_suggestions();
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    terminal.backend_mut().flush()?;
    crossterm::execute!(terminal.backend_mut(), DisableBracketedPaste)?;
    drop(terminal);
    disable_raw_mode()?;
    // Flush any session entries buffered by the persist throttle.
    #[cfg(feature = "pi-compat")]
    {
        let _ = app.persist();
    }
    Ok(())
}

pub(crate) fn truncate_args(args: &str, max: usize) -> String {
    let flat = args.replace('\n', " ");
    if flat.chars().count() <= max {
        flat
    } else {
        let mut out: String = flat.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

pub(crate) fn bounded_plan_preview(proposal: &PlanProposal) -> Vec<String> {
    let rendered = proposal.render();
    let mut rows: Vec<String> = rendered
        .lines()
        .take(PLAN_PREVIEW_MAX_LINES)
        .map(|line| clean_search_text(line, PLAN_PREVIEW_LINE_LIMIT))
        .collect();
    if rendered.lines().nth(PLAN_PREVIEW_MAX_LINES).is_some() {
        rows.push("… (plan preview truncated)".to_string());
    }
    rows
}

pub(crate) fn tool_detail(name: &str, arguments: &str) -> String {
    let Ok(arguments) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return truncate_args(arguments, 120);
    };
    let key = match name {
        "bash" => "command",
        "grep" | "find" => "pattern",
        _ => "path",
    };
    arguments
        .get(key)
        .or_else(|| arguments.get("name"))
        .and_then(|value| value.as_str())
        .map(|value| truncate_args(value, 120))
        .unwrap_or_default()
}

pub(crate) fn tool_result_summary(name: &str, content: &str, is_error: bool) -> String {
    if is_error {
        return content
            .lines()
            .find(|line| !line.trim().is_empty())
            .map(|line| truncate_args(line, 120))
            .unwrap_or_else(|| "error".to_string());
    }
    let count = content.lines().filter(|line| !line.is_empty()).count();
    match name {
        "read" => format!("{count} lines"),
        "grep" => format!("{count} matches"),
        "find" => format!("{count} files"),
        "ls" => format!("{count} entries"),
        "write" => "written".to_string(),
        "edit" => "applied".to_string(),
        "bash" => content
            .lines()
            .rev()
            .find_map(|line| {
                line.trim()
                    .strip_prefix("(exit code: ")
                    .and_then(|code| code.strip_suffix(')'))
                    .map(|code| format!("failed · exit {code}"))
            })
            .or_else(|| {
                content
                    .lines()
                    .find(|line| !line.trim().is_empty() && *line != "(no output)")
                    .map(|line| truncate_args(line.trim(), 120))
            })
            .unwrap_or_else(|| "done".to_string()),
        _ if count == 0 => "done".to_string(),
        _ => format!("{count} results"),
    }
}

pub(crate) fn is_permission_toggle(code: KeyCode, modifiers: KeyModifiers) -> bool {
    code == KeyCode::Char('~')
        || code == KeyCode::Char('`') && modifiers.contains(KeyModifiers::SHIFT)
}

pub(crate) fn tool_color(name: &str) -> crepuscularity_tui::ratatui::style::Color {
    use crepuscularity_tui::ratatui::style::Color;
    match name {
        "read" | "grep" | "find" | "ls" => Color::Cyan,
        "write" | "edit" => Color::Yellow,
        "bash" => Color::Magenta,
        _ => Color::Blue,
    }
}

pub(crate) fn wrap_scrollback_line(
    prefix: &str,
    text: &str,
    width: usize,
    color: crepuscularity_tui::ratatui::style::Color,
) -> Vec<Line<'static>> {
    use crepuscularity_tui::ratatui::style::Style;

    let prefix_width = prefix.chars().count();
    let content_width = width.saturating_sub(prefix_width).max(1);
    let chars = text.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return vec![Line::styled(prefix.to_string(), Style::default().fg(color))];
    }
    let mut lines = Vec::new();
    let mut remaining = chars.as_slice();
    while !remaining.is_empty() {
        let split = if remaining.len() <= content_width {
            remaining.len()
        } else {
            remaining[..content_width]
                .iter()
                .rposition(|ch| ch.is_whitespace())
                .filter(|index| *index > 0)
                .unwrap_or(content_width)
        };
        let chunk = remaining[..split].iter().collect::<String>();
        remaining = &remaining[split..];
        remaining = &remaining[remaining.iter().take_while(|ch| ch.is_whitespace()).count()..];
        let indent = if lines.is_empty() {
            prefix.to_string()
        } else {
            " ".repeat(prefix_width)
        };
        lines.push(Line::styled(
            format!("{indent}{chunk}"),
            Style::default().fg(color),
        ));
    }
    lines
}
