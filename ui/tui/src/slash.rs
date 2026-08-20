use std::path::PathBuf;
use std::sync::Arc;

use darash::SearchResponse;
use rx4::agent::Agent;
use rx4::mode::Scope;
use rx4::subagent::SubagentConfig;
use tokio::sync::Mutex;

use crate::app::{
    slash_description, App, AppEvent, ChatMessage, MAX_BUDGET_DURATION_SECONDS, MAX_BUDGET_TURNS,
};
use crate::host::{apply_scope, parse_host_scope, scope_usage};
use crate::mcp_config;
#[cfg(feature = "pi-compat")]
use crate::pi::{self, PiEntryType, PiSession};
use crate::provider_catalog;
use crate::providers::{api_key_help, providers_summary, push_system_message, run_login_from_tui};
#[cfg(feature = "pi-compat")]
use crate::tui::{restored_chat, session_files};

pub(crate) fn plan_request(task: &str) -> String {
    format!(
        "Create a concrete implementation plan for: {task}\n\nInspect the relevant code and instructions first. Return the files to change, the ordered steps, risks, and verification commands. Do not modify the workspace."
    )
}

pub(crate) fn review_request(target: &str) -> String {
    format!(
        "Review {target} for correctness, security, regressions, and missing verification. Inspect the repository before reporting. Do not modify the workspace. Return only actionable findings, ordered by severity, with file paths and concise evidence; say explicitly when there are no findings."
    )
}

pub(crate) fn budget_summary(agent: &Agent) -> String {
    match &agent.budget {
        Some(budget) => format!(
            "Budget: max_cost={:?}, max_duration={:?}s, max_turns={}",
            budget.max_cost, budget.max_duration_seconds, agent.max_tool_iterations
        ),
        None => format!(
            "No cost/time budget set; max_turns={}",
            agent.max_tool_iterations
        ),
    }
}

pub(crate) fn apply_budget_command(agent: &mut Agent, arg: &str) -> String {
    let words: Vec<&str> = arg.split_whitespace().collect();
    if words == ["clear"] {
        agent.budget = None;
        agent.max_tool_iterations = rx4::guardrails::MAX_TOOL_ITERATIONS_DEFAULT;
        return format!(
            "Budget cleared; max_turns reset to {}.",
            rx4::guardrails::MAX_TOOL_ITERATIONS_DEFAULT
        );
    }

    let (kind, value) = match words.as_slice() {
        [value] => ("cost", *value),
        [kind, value] => (*kind, *value),
        _ => {
            return "Usage: /budget [<cost>|cost <usd>|time <seconds>|turns <count>|clear]"
                .to_string()
        }
    };

    match kind {
        "cost" => match value.parse::<f64>() {
            Ok(cost) if cost.is_finite() && cost > 0.0 => {
                let mut budget = agent.budget.clone().unwrap_or_default();
                budget.max_cost = Some(cost);
                agent.budget = Some(budget);
                format!("Budget max_cost set to ${cost:.4}")
            }
            _ => "Invalid cost; use a positive finite USD amount.".to_string(),
        },
        "time" | "duration" => match value.parse::<u64>() {
            Ok(seconds) if seconds > 0 => {
                let capped = seconds.min(MAX_BUDGET_DURATION_SECONDS);
                let mut budget = agent.budget.clone().unwrap_or_default();
                budget.max_duration_seconds = Some(capped);
                agent.budget = Some(budget);
                if capped == seconds {
                    format!("Budget max_duration set to {capped}s")
                } else {
                    format!(
                        "Budget max_duration capped at {MAX_BUDGET_DURATION_SECONDS}s (requested {seconds}s)"
                    )
                }
            }
            _ => "Invalid duration; use a positive number of seconds.".to_string(),
        },
        "turns" => match value.parse::<usize>() {
            Ok(turns) if turns > 0 => {
                let capped = turns.min(MAX_BUDGET_TURNS);
                agent.max_tool_iterations = capped;
                if capped == turns {
                    format!("Budget max_turns set to {capped}")
                } else {
                    format!("Budget max_turns capped at {MAX_BUDGET_TURNS} (requested {turns})")
                }
            }
            _ => "Invalid turns; use a positive integer.".to_string(),
        },
        _ => "Usage: /budget [<cost>|cost <usd>|time <seconds>|turns <count>|clear]".to_string(),
    }
}

pub(crate) const SEARCH_RESULT_LIMIT: usize = 8;
pub(crate) const SEARCH_TEXT_LIMIT: usize = 600;

pub(crate) fn clean_search_text(value: &str, limit: usize) -> String {
    let mut text: String = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    if text.chars().count() > limit {
        text = text.chars().take(limit.saturating_sub(1)).collect();
        text.push('…');
    }
    text
}

pub(crate) fn format_search_response(query: &str, response: &SearchResponse) -> String {
    let citations = response.cited_sources();
    let mut lines = vec![format!(
        "Search results for {:?} (showing {} of {} cited sources, {} total results)",
        clean_search_text(query, SEARCH_TEXT_LIMIT),
        citations.len().min(SEARCH_RESULT_LIMIT),
        citations.len(),
        response.number_of_results
    )];
    for (index, source) in citations.iter().take(SEARCH_RESULT_LIMIT).enumerate() {
        let title = clean_search_text(&source.title, SEARCH_TEXT_LIMIT);
        let title = if title.is_empty() {
            "(untitled)".to_string()
        } else {
            title
        };
        lines.push(format!(
            "\n[{}] {title}\nURL: {}\n{}",
            index + 1,
            clean_search_text(&source.url, SEARCH_TEXT_LIMIT),
            clean_search_text(&source.snippet, SEARCH_TEXT_LIMIT)
        ));
    }
    if let Some(answer) = &response.answer {
        lines.push(format!(
            "Answer: {}",
            clean_search_text(answer, SEARCH_TEXT_LIMIT)
        ));
    } else if !response.answers.is_empty() {
        lines.push(format!(
            "Answer: {}",
            clean_search_text(&response.answers.join("; "), SEARCH_TEXT_LIMIT)
        ));
    }
    if !response.suggestions.is_empty() {
        lines.push(format!(
            "Suggestions: {}",
            clean_search_text(&response.suggestions.join(", "), SEARCH_TEXT_LIMIT)
        ));
    }
    lines.join("\n")
}

pub(crate) fn handle_slash_command(
    app: &mut App,
    cmd: &str,
    agent: &Arc<Mutex<Agent>>,
    tx: &tokio::sync::mpsc::UnboundedSender<AppEvent>,
) {
    let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
    let command = parts[0];
    let arg = parts.get(1).copied().unwrap_or("");
    app.clear_input();
    app.slash_suggestions.clear();

    match command {
        "/quit" | "/exit" => {}
        "/clear" => {
            if let Some(a) = &app.agent {
                if let Ok(agent) = a.try_lock() {
                    app.cost_baseline = agent.total_cost();
                }
            }
            app.messages.clear();
            app.input_tokens = 0;
            app.output_tokens = 0;
            app.cache_read_tokens = 0;
            app.cache_write_tokens = 0;
            app.cost = 0.0;
        }
        "/help" | "/commands" => {
            if command == "/commands" && !arg.is_empty() {
                let name = if arg.starts_with('/') {
                    arg.to_string()
                } else {
                    format!("/{arg}")
                };
                let description = slash_description(&name);
                push_system_message(
                    app,
                    if description.is_empty() {
                        format!("Unknown command: {name}. Type /commands to list commands.")
                    } else {
                        format!("{name} — {description}")
                    },
                );
                return;
            }
            app.messages.push(ChatMessage {
                role: "system".to_string(),
                content: format!(
                    "Commands: /providers (or /provider, /auth), /apikey <provider> (or /keys), /login [provider], /config, /model [name], {}, /plan <task>, /review [target], /sessions, /resume <n>, /subagent spawn|list|cancel, /budget [<cost>|cost <usd>|time <seconds>|turns <count>|clear], /plan-approval ask|bypass|off, /mcp, /todo, /clear, /cost, /commands, /help, /quit\nKeys: / command suggestions (with descriptions): Up/Down select, Tab insert, Enter apply · /providers: type search, Enter details, Esc cancel · /model <partial> completes model names · model selector: type search (fuzzy, cross-provider), Left/Right provider, Up/Down model, Enter apply, Esc cancel · config menu: Up/Down select, Enter apply, Esc close · ←/→ cursor, Ctrl/Alt+←/→ word, Ctrl+A/E line start/end, Ctrl+K/U delete to end/start, Ctrl+W delete word, Ctrl+Z undo, Home/End line start/end · Alt+Shift+←/→ scope · Shift+Tab effort · Shift+Enter newline · Esc/Ctrl+C interrupt (Ctrl+C clears draft) · Ctrl+B header · Ctrl+L clear",
                    scope_usage().replacen("Usage: ", "", 1)
                ),
                is_tool: false,
                tool_name: String::new(),
                tool_call_id: String::new(),
                is_streaming: false,
            });
        }
        "/login" => {
            let provider = (!arg.is_empty()).then_some(arg);
            let result = run_login_from_tui(provider);
            push_system_message(
                app,
                match result {
                    Ok(()) => "Login complete. Restart tk to load the new provider.".to_string(),
                    Err(error) => format!("Login failed: {error}"),
                },
            );
        }
        "/providers" | "/provider" | "/auth" => {
            if arg.is_empty() {
                app.open_provider_menu();
            } else if let Some(provider) = provider_catalog::find(arg) {
                push_system_message(app, api_key_help(provider));
            } else {
                push_system_message(
                    app,
                    format!("Unknown provider: {arg}. Use /providers and type to search."),
                );
            }
        }
        "/apikey" | "/keys" => {
            if let Some(provider) = provider_catalog::find(arg) {
                push_system_message(app, api_key_help(provider));
            } else if arg.is_empty() {
                app.open_provider_menu();
            } else {
                push_system_message(
                    app,
                    format!("Unknown API-key provider: {arg}. Use /providers and type to search."),
                );
            }
        }
        "/config" => {
            let config_parts: Vec<&str> = arg.splitn(2, ' ').collect();
            let subcommand = config_parts.first().copied().unwrap_or("");
            let rest = config_parts.get(1).copied().unwrap_or("");
            match subcommand {
                // No subcommand: open the interactive config menu (QoL).
                "" => {
                    app.open_config();
                }
                "show" => {
                    let summary = providers_summary(app);
                    push_system_message(app, summary);
                }
                "login" => {
                    let provider = (!rest.is_empty()).then_some(rest);
                    let result = run_login_from_tui(provider);
                    push_system_message(
                        app,
                        match result {
                            Ok(()) => "Login complete. Restart tk to load the new provider."
                                .to_string(),
                            Err(error) => format!("Login failed: {error}"),
                        },
                    );
                }
                "model" if !rest.is_empty() => {
                    handle_slash_command(app, &format!("/model {rest}"), agent, tx);
                }
                "scope" if !rest.is_empty() => {
                    handle_slash_command(app, &format!("/scope {rest}"), agent, tx);
                }
                _ => push_system_message(
                    app,
                    "Usage: /config | /config show | /config login [provider] | /config model <name> | /config scope <name>",
                ),
            }
        }
        "/model" => {
            if arg.is_empty() {
                app.open_model_selector();
                app.refresh_remote_model_choices(tx.clone());
            } else {
                #[cfg(feature = "pi-compat")]
                app.append_session(PiEntryType::ModelChange {
                    from: app.model.clone(),
                    to: arg.to_string(),
                });
                let model = arg.to_string();
                app.set_model(model.clone());
                if let Some(a) = &app.agent {
                    if let Ok(mut agent) = a.try_lock() {
                        agent.set_model(model);
                    }
                }
                app.messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: format!("Model set to: {arg}"),
                    is_tool: false,
                    tool_name: String::new(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
            }
        }
        "/cost" => {
            app.refresh_cost();
            app.messages.push(ChatMessage {
                role: "system".to_string(),
                content: format!(
                    "Input: {} tokens (cached reads: {}, cache writes: {}), Output: {} tokens, Cache hit: {:.1}%, Cost: ${:.4}",
                    app.input_tokens,
                    app.cache_read_tokens,
                    app.cache_write_tokens,
                    app.output_tokens,
                    if app.input_tokens == 0 { 0.0 } else { app.cache_read_tokens as f64 * 100.0 / app.input_tokens as f64 },
                    app.cost
                ),
                is_tool: false,
                tool_name: String::new(),
                tool_call_id: String::new(),
                is_streaming: false,
            });
        }
        "/sessions" => {
            #[cfg(feature = "pi-compat")]
            {
                let files = session_files();
                if files.is_empty() {
                    push_system_message(
                        app,
                        "No sessions yet. Start a conversation to create one.",
                    );
                    return;
                }
                let lines = files
                    .iter()
                    .enumerate()
                    .map(|(index, path)| {
                        let session = PiSession::load_jsonl(path).ok();
                        let model = session
                            .as_ref()
                            .map(|session| session.header.model.clone())
                            .unwrap_or_else(|| "?".to_string());
                        let count = session
                            .as_ref()
                            .map(|session| session.message_count())
                            .unwrap_or(0);
                        let stamp = std::fs::metadata(path)
                            .and_then(|metadata| metadata.modified())
                            .ok()
                            .map(|modified| {
                                chrono::DateTime::<chrono::Utc>::from(modified)
                                    .format("%m-%d %H:%M")
                                    .to_string()
                            })
                            .unwrap_or_else(|| "?".to_string());
                        let name = path
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        format!("  [{index}] {stamp} · {model} · {count} messages · {name}")
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                push_system_message(
                    app,
                    format!("Sessions (newest first)\n{lines}\n\nResume with /resume <n>"),
                );
            }
            #[cfg(not(feature = "pi-compat"))]
            {
                push_system_message(app, "Sessions require the pi-compat feature.");
            }
        }
        "/resume" => {
            #[cfg(feature = "pi-compat")]
            {
                let Ok(index) = arg.parse::<usize>() else {
                    push_system_message(app, "Usage: /resume <n> — list sessions with /sessions");
                    return;
                };
                let files = session_files();
                let Some(path) = files.get(index.wrapping_sub(1)) else {
                    push_system_message(
                        app,
                        format!("No session {index}. List sessions with /sessions."),
                    );
                    return;
                };
                match PiSession::load_jsonl(path) {
                    Ok(session) => {
                        app.messages = restored_chat(&session);
                        let messages = session.messages();
                        let dir = pi::pi_sessions_dir(&std::env::current_dir().unwrap_or_default());
                        app.session = Some((session, dir));
                        if let Some(agent) = &app.agent {
                            if let Ok(agent) = agent.try_lock() {
                                *agent.messages.write() = messages;
                            }
                        }
                        let _ = app.persist();
                        push_system_message(
                            app,
                            format!(
                                "Resumed session {}",
                                path.file_name()
                                    .map(|name| name.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| path.display().to_string())
                            ),
                        );
                    }
                    Err(error) => {
                        push_system_message(app, format!("Failed to load session: {error}"));
                    }
                }
            }
            #[cfg(not(feature = "pi-compat"))]
            {
                push_system_message(app, "Sessions require the pi-compat feature.");
            }
        }
        "/scope" => {
            let scope = match parse_host_scope(arg) {
                Ok(scope) => scope,
                Err(message) => {
                    push_system_message(app, message);
                    return;
                }
            };
            if let Ok(mut agent) = agent.try_lock() {
                apply_scope(&mut agent, scope);
            }
            app.agent_mode = scope.name().to_string();
            app.persist_prefs();
            app.messages.push(ChatMessage {
                role: "system".to_string(),
                content: format!("Scope set to: {}", scope.name()),
                is_tool: false,
                tool_name: String::new(),
                tool_call_id: String::new(),
                is_streaming: false,
            });
        }
        "/plan" => {
            if arg.is_empty() {
                app.messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: "Usage: /plan <task>".to_string(),
                    is_tool: false,
                    tool_name: String::new(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
                return;
            }
            if let Ok(mut agent) = agent.try_lock() {
                apply_scope(&mut agent, Scope::Plan);
            }
            app.agent_mode = Scope::Plan.name().to_string();
            app.persist_prefs();
            app.input = plan_request(arg);
            app.submit_prompt(agent, tx.clone());
        }
        "/review" => {
            if let Ok(mut agent) = agent.try_lock() {
                apply_scope(&mut agent, Scope::Research);
            }
            app.agent_mode = Scope::Research.name().to_string();
            app.persist_prefs();
            let target = if arg.is_empty() {
                "the current workspace"
            } else {
                arg
            };
            app.input = review_request(target);
            app.submit_prompt(agent, tx.clone());
        }
        "/mcp" => {
            let path = mcp_config::config_path()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "(no home directory; MCP config not loaded)".to_string());
            let body = if app.mcp_connecting {
                format!("Connecting to MCP servers…\nConfig: {path}")
            } else if app.mcp_tools.is_empty() {
                format!(
                    "No MCP tools connected.\nConfig: {path}\nFormat: {{\"servers\":[{{\"name\":\"fs\",\"transport\":\"stdio\",\"command\":\"npx\",\"args\":[\"-y\",\"@modelcontextprotocol/server-filesystem\",\".\"]}}]}}\nRemote HTTP/SSE: put url+transport=http|sse in config (host loader documents it; engine stdio works today)."
                )
            } else {
                format!(
                    "MCP tools ({}):\n{}\nConfig: {path}",
                    app.mcp_tools.len(),
                    app.mcp_tools.join("\n"),
                )
            };
            app.messages.push(ChatMessage {
                role: "system".to_string(),
                content: body,
                is_tool: false,
                tool_name: String::new(),
                tool_call_id: String::new(),
                is_streaming: false,
            });
        }
        "/todo" => {
            app.messages.push(ChatMessage {
                role: "system".to_string(),
                content: "/todo: host surface only. Engine may expose todo tool later — track work in chat or project TODO for now.".to_string(),
                is_tool: false,
                tool_name: String::new(),
                tool_call_id: String::new(),
                is_streaming: false,
            });
        }
        "/budget" => {
            let msg = if let Some(a) = &app.agent {
                if let Ok(mut agent) = a.try_lock() {
                    if arg.is_empty() {
                        budget_summary(&agent)
                    } else {
                        apply_budget_command(&mut agent, arg)
                    }
                } else {
                    "Agent busy; retry the budget command when idle.".to_string()
                }
            } else {
                "No agent.".to_string()
            };
            push_system_message(app, msg);
        }
        "/plan-approval" => match arg {
            "ask" | "on" => {
                if app.plan_prompt {
                    app.resolve_plan(false);
                }
                if let Ok(mut agent) = agent.try_lock() {
                    let (plan_approver, plan_rx) = rx4::permissions::ChannelPlanApprover::pair();
                    agent.set_plan_approver(Arc::new(plan_approver));
                    app.plan_rx = Some(plan_rx);
                    push_system_message(
                        app,
                        "Whole-turn plan approval enabled (y approve, n reject).",
                    );
                } else {
                    push_system_message(app, "Agent busy; retry /plan-approval ask when idle.");
                }
            }
            "bypass" | "allow" => {
                if app.plan_prompt {
                    app.resolve_plan(false);
                }
                if let Ok(mut agent) = agent.try_lock() {
                    agent.set_plan_approver(Arc::new(rx4::permissions::AlwaysApprovePlan));
                    app.plan_rx = None;
                    push_system_message(app, "Whole-turn plan approval bypassed.");
                } else {
                    push_system_message(app, "Agent busy; retry /plan-approval bypass when idle.");
                }
            }
            "off" | "disable" => {
                if app.plan_prompt {
                    app.resolve_plan(false);
                }
                if let Ok(mut agent) = agent.try_lock() {
                    agent.clear_plan_approver();
                    app.plan_rx = None;
                    push_system_message(app, "Whole-turn plan approval disabled.");
                } else {
                    push_system_message(app, "Agent busy; retry /plan-approval off when idle.");
                }
            }
            "" => push_system_message(
                app,
                if app.plan_rx.is_some() {
                    "Whole-turn plan approval: ask (y approve, n reject)."
                } else {
                    "Whole-turn plan approval: bypassed or disabled. Use /plan-approval ask|bypass|off."
                },
            ),
            _ => push_system_message(app, "Usage: /plan-approval ask|bypass|off"),
        },
        "/subagent" => {
            let sub_parts: Vec<&str> = arg.splitn(2, ' ').collect();
            let sub = sub_parts.first().copied().unwrap_or("");
            let rest = sub_parts.get(1).copied().unwrap_or("");
            match sub {
                "spawn" => {
                    if let Some(mgr) = app.subagent_manager.clone() {
                        let prompt = rest.to_string();
                        let name = prompt
                            .split_whitespace()
                            .next()
                            .unwrap_or("subagent")
                            .to_string();
                        app.messages.push(ChatMessage {
                            role: "system".to_string(),
                            content: format!("Spawning subagent '{name}'..."),
                            is_tool: false,
                            tool_name: String::new(),
                            tool_call_id: String::new(),
                            is_streaming: false,
                        });
                        let workspace =
                            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                        let result = mgr.lock().spawn_background(
                            SubagentConfig {
                                name: name.clone(),
                                workspace_isolation: true,
                                ..SubagentConfig::default()
                            },
                            &prompt,
                            &workspace,
                        );
                        app.messages.push(ChatMessage {
                            role: "system".to_string(),
                            content: match result {
                                Ok(handle) => {
                                    format!("Subagent {name} running — id: {}", handle.id())
                                }
                                Err(error) => format!("Subagent error: {error}"),
                            },
                            is_tool: false,
                            tool_name: String::new(),
                            tool_call_id: String::new(),
                            is_streaming: false,
                        });
                    } else {
                        app.messages.push(ChatMessage {
                            role: "system".to_string(),
                            content: "Subagent manager not initialized.".to_string(),
                            is_tool: false,
                            tool_name: String::new(),
                            tool_call_id: String::new(),
                            is_streaming: false,
                        });
                    }
                }
                "list" => {
                    if let Some(mgr) = app.subagent_manager.as_ref() {
                        let mgr = mgr.lock();
                        let handles = mgr.list();
                        let body = if handles.is_empty() {
                            "No subagents.".to_string()
                        } else {
                            handles
                                .iter()
                                .map(|h| {
                                    format!(
                                        "{}: {} [{:?}] depth={} children={} descendants={}",
                                        h.id(),
                                        h.name(),
                                        h.status(),
                                        h.depth(),
                                        h.children().len(),
                                        h.descendant_count()
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("\n")
                        };
                        app.messages.push(ChatMessage {
                            role: "system".to_string(),
                            content: body,
                            is_tool: false,
                            tool_name: String::new(),
                            tool_call_id: String::new(),
                            is_streaming: false,
                        });
                    }
                }
                "cancel" => {
                    if rest.is_empty() {
                        app.messages.push(ChatMessage {
                            role: "system".to_string(),
                            content: "Usage: /subagent cancel <id>".to_string(),
                            is_tool: false,
                            tool_name: String::new(),
                            tool_call_id: String::new(),
                            is_streaming: false,
                        });
                    } else if let Some(mgr) = app.subagent_manager.as_ref() {
                        let body = match mgr.lock().cancel(rest) {
                            Ok(()) => format!("Cancelled subagent {rest}."),
                            Err(e) => format!("Cancel failed: {e}"),
                        };
                        app.messages.push(ChatMessage {
                            role: "system".to_string(),
                            content: body,
                            is_tool: false,
                            tool_name: String::new(),
                            tool_call_id: String::new(),
                            is_streaming: false,
                        });
                    }
                }
                _ => {
                    app.messages.push(ChatMessage {
                        role: "system".to_string(),
                        content: "Usage: /subagent spawn <prompt> | list | cancel <id>".to_string(),
                        is_tool: false,
                        tool_name: String::new(),
                        tool_call_id: String::new(),
                        is_streaming: false,
                    });
                }
            }
        }
        _ => {
            app.messages.push(ChatMessage {
                role: "system".to_string(),
                content: format!("Unknown command: {command}. Type /help for available commands."),
                is_tool: false,
                tool_name: String::new(),
                tool_call_id: String::new(),
                is_streaming: false,
            });
        }
    }
}
