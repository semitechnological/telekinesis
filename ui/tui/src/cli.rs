use std::io::IsTerminal;
use std::path::PathBuf;

use crate::exec::{run_exec, ExecArgs};
use crate::providers::run_login;
use crate::tui::run_tui;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Login { provider: Option<String> },
    Help,
    Exec(ExecArgs),
    Tui { continue_session: bool },
    Headless(ExecArgs),
}

pub fn is_continue_arg(arg: &str) -> bool {
    arg == "-c" || arg == "--continue"
}

pub fn parse_exec_args(args: &[String]) -> Result<ExecArgs, String> {
    parse_run_args(args, true)
}

pub fn parse_implicit_args(args: &[String]) -> Result<ExecArgs, String> {
    parse_run_args(args, false)
}

fn parse_run_args(args: &[String], allow_prompt: bool) -> Result<ExecArgs, String> {
    let mut parsed = ExecArgs::default();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "--help" | "-h" => parsed.help = true,
            "--json" => parsed.json = true,
            "--no-yolo" => parsed.no_yolo = true,
            "--model" => {
                index += 1;
                let model = args
                    .get(index)
                    .ok_or_else(|| "--model requires a name".to_string())?;
                if model.is_empty() {
                    return Err("--model requires a name".to_string());
                }
                parsed.model = Some(model.clone());
            }
            "--cwd" => {
                index += 1;
                let dir = args
                    .get(index)
                    .ok_or_else(|| "--cwd requires a directory".to_string())?;
                parsed.cwd = Some(PathBuf::from(dir));
            }
            "-" if allow_prompt => parsed.prompt = None,
            _ if is_continue_arg(arg) => {}
            _ if arg.starts_with("--") => return Err(format!("Unknown option: {arg}")),
            _ if allow_prompt => {
                if parsed.prompt.is_some() {
                    return Err(format!("Unexpected extra argument: {arg}"));
                }
                parsed.prompt = Some(arg.to_string());
            }
            _ => {
                return Err(format!(
                    "Unexpected extra argument: {arg}; pipe a prompt on stdin or use tk exec"
                ));
            }
        }
        index += 1;
    }
    Ok(parsed)
}

pub fn parse_command(args: &[String], interactive: bool) -> Result<Command, String> {
    let rest = if args.len() > 1 { &args[1..] } else { &[] };
    match rest.first().map(String::as_str) {
        Some("login") => Ok(Command::Login {
            provider: rest.get(1).cloned(),
        }),
        Some("exec") => Ok(Command::Exec(parse_exec_args(&rest[1..])?)),
        Some("--help") | Some("-h") => Ok(Command::Help),
        _ if interactive => Ok(Command::Tui {
            continue_session: rest.iter().any(|arg| is_continue_arg(arg)),
        }),
        _ => Ok(Command::Headless(parse_implicit_args(rest)?)),
    }
}

pub fn print_help() {
    println!("telekinesis (tk) — AI coding agent TUI");
    println!();
    println!("USAGE:");
    println!("  tk              Start interactive TUI");
    println!("  tk -c           Continue newest session for this project");
    println!("  tk exec \"<prompt>\"   Run one turn headlessly, final text on stdout");
    println!(
        "                       (prompt from stdin with `-`; --json, --cwd <dir>, --model <name>, --no-yolo)"
    );
    println!("  tk --no-yolo    Headless stdin run that denies Ask-class tools");
    println!(
        "  tk login <provider>  OAuth login (openai, claude, grok, gemini, copilot, kimi, antigravity)"
    );
    println!("  /login [provider]     OAuth login from the TUI");
    println!("  /providers             Search providers and API-key setup");
    println!("  /apikey <provider>     Show one provider's API-key setup");
    println!("  /config               Interactive config menu");
    println!("  /config show          Show runtime configuration and auth status");
    println!("  /sessions /resume <n> List and switch JSONL sessions");
    println!("  tk --help       Show this help");
    println!();
    println!("ENVIRONMENT:");
    println!("  XAI_API_KEY         xAI Grok API key");
    println!("  OPENAI_API_KEY      OpenAI API key");
    println!("  ANTHROPIC_API_KEY   Anthropic API key");
    println!("  GOOGLE_API_KEY      Google Gemini API key");
    println!("  OPENCODE_API_KEY    OpenCode Zen / OpenCode Go API key");
    println!("  OPENROUTER_API_KEY  OpenRouter API key");
    println!("  TK_PLAN_APPROVAL    ask (default), off, or bypass whole-turn plans");
    println!("  TK_TOOL_PROFILE     minimal, coding, or full tool registry");
    println!("                      (cu_* needs --features computer-use or full)");
    println!("                      (MCP needs --features mcp or full)");
    println!("                      (web_search needs --features search or full)");
    println!();
    println!("KEYS:");
    println!("  Enter        Submit prompt");
    println!("  Shift+Enter  New line");
    println!("  Esc/Ctrl+C   Interrupt; Ctrl+C clears draft, again exits");
    println!("  Ctrl+L       Clear screen");
    println!("  Ctrl+B       Toggle header");
    println!("  F1           Show help");
    println!("  ←/→          Move cursor · Ctrl/Alt+←/→ word · Home/End line");
    println!("  Ctrl+A/E/K/U/W  Line editing (start/end, delete to end/start, delete word)");
    println!("  Shift+Tab    Cycle reasoning effort");
    println!("  Alt+Shift+←/→ Cycle agent scope");
    println!("  Up/Down      Input history");
    println!("  PgUp/PgDn    Scroll chat view");
    println!("  Home/End     Jump to top/bottom of chat");
}

pub fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    match parse_command(&args, interactive) {
        Ok(Command::Login { provider }) => run_login(provider.as_deref()),
        Ok(Command::Help) => {
            print_help();
            Ok(())
        }
        Ok(Command::Exec(exec)) => run_exec(exec),
        Ok(Command::Tui { continue_session }) => run_tui(continue_session),
        Ok(Command::Headless(exec)) => run_exec(exec),
        Err(message) => {
            eprintln!("error: {message}");
            print_help();
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        std::iter::once("tk".to_string())
            .chain(values.iter().map(|value| value.to_string()))
            .collect()
    }

    #[test]
    fn exec_reads_prompt_from_argument_or_stdin() {
        let parsed = parse_exec_args(&["say hi".to_string()]).unwrap();
        assert_eq!(parsed.prompt.as_deref(), Some("say hi"));
        assert!(!parsed.no_yolo);
        assert_eq!(parse_exec_args(&["-".to_string()]).unwrap().prompt, None);
        assert_eq!(parse_exec_args(&[]).unwrap().prompt, None);
    }

    #[test]
    fn exec_parses_json_cwd_no_yolo_and_rejects_junk() {
        let parsed = parse_exec_args(&[
            "--json".into(),
            "--cwd".into(),
            "/tmp".into(),
            "task".into(),
        ])
        .unwrap();
        assert!(parsed.json);
        assert_eq!(parsed.cwd, Some(PathBuf::from("/tmp")));
        assert_eq!(parsed.prompt.as_deref(), Some("task"));
        assert!(!parsed.no_yolo);
        assert_eq!(parsed.model, None);
        let no_yolo = parse_exec_args(&["--no-yolo".into(), "task".into()]).unwrap();
        assert!(no_yolo.no_yolo);
        assert!(parse_exec_args(&["--cwd".to_string()]).is_err());
        assert!(parse_exec_args(&["--nope".to_string()]).is_err());
        assert!(parse_exec_args(&["a".to_string(), "b".to_string()]).is_err());
    }

    #[test]
    fn exec_parses_model_for_avo_driver_and_supervisor() {
        let parsed =
            parse_exec_args(&["--model".into(), "grok-4.5".into(), "task".into()]).unwrap();
        assert_eq!(parsed.model.as_deref(), Some("grok-4.5"));
        assert_eq!(parsed.prompt.as_deref(), Some("task"));
        assert!(parse_exec_args(&["--model".into()]).is_err());
        assert!(parse_exec_args(&["--model".into(), "".into(), "task".into()]).is_err());
    }

    #[test]
    fn implicit_headless_reads_flags_not_leftover_words() {
        let parsed = parse_implicit_args(&[
            "--json".into(),
            "--cwd".into(),
            "/tmp".into(),
            "--no-yolo".into(),
        ])
        .unwrap();
        assert!(parsed.json);
        assert!(parsed.no_yolo);
        assert_eq!(parsed.cwd, Some(PathBuf::from("/tmp")));
        assert_eq!(parsed.prompt, None);
        assert!(parse_implicit_args(&["hello".into()]).is_err());
        assert!(parse_implicit_args(&["-c".into()])
            .unwrap()
            .prompt
            .is_none());
        assert!(parse_implicit_args(&["--continue".into()])
            .unwrap()
            .prompt
            .is_none());
    }

    #[test]
    fn parse_command_routes_tty_and_headless() {
        assert_eq!(
            parse_command(&args(&["login", "grok"]), true).unwrap(),
            Command::Login {
                provider: Some("grok".into())
            }
        );
        assert!(matches!(
            parse_command(&args(&["-c"]), true).unwrap(),
            Command::Tui {
                continue_session: true
            }
        ));
        assert!(matches!(
            parse_command(&args(&["--json"]), false).unwrap(),
            Command::Headless(exec) if exec.json && exec.prompt.is_none()
        ));
        assert!(matches!(
            parse_command(&args(&["exec", "--no-yolo", "go"]), false).unwrap(),
            Command::Exec(exec) if exec.no_yolo && exec.prompt.as_deref() == Some("go")
        ));
    }

    #[test]
    fn continue_accepts_short_and_long_flags() {
        assert!(is_continue_arg("-c"));
        assert!(is_continue_arg("--continue"));
        assert!(!is_continue_arg("-C"));
    }
}
