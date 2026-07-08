use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Html,
    routing::get,
    Router,
};
use crepuscularity_core::{TemplateContext, TemplateValue};
use serde::Deserialize;

struct AppState {
    template: String,
    socket_path: PathBuf,
}

#[derive(Deserialize)]
struct ChatQuery {
    text: Option<String>,
}

async fn index_handler(State(state): State<Arc<AppState>>) -> Html<String> {
    let mut ctx = TemplateContext::new();
    ctx.set("model", "gpt-4o");
    ctx.set("tools_count", 6);
    ctx.set("plugins_count", 2);
    ctx.set("status", "Connected");
    ctx.set("message_count", 0);
    ctx.set("busy", false);
    ctx.set("input", "");

    let msgs: Vec<TemplateContext> = Vec::new();
    ctx.set("messages", TemplateValue::List(msgs));

    let html = crepuscularity_web::render_template_to_html(&state.template, &ctx).unwrap_or_else(|e| {
        format!("<html><body><h1>Template error</h1><pre>{e}</pre></body></html>")
    });

    Html(html)
}

async fn chat_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ChatQuery>,
) -> Result<Html<String>, StatusCode> {
    let text = query.text.unwrap_or_default();

    // Connect to IPC server and send prompt
    let response = send_ipc_prompt(&state.socket_path, &text).await.unwrap_or_else(|e| format!("Error: {e}"));

    let mut ctx = TemplateContext::new();
    ctx.set("model", "gpt-4o");
    ctx.set("tools_count", 6);
    ctx.set("plugins_count", 2);
    ctx.set("status", "Ready");
    ctx.set("message_count", 1);
    ctx.set("busy", false);
    ctx.set("input", "");

    let mut msgs: Vec<TemplateContext> = Vec::new();
    let mut user_msg = TemplateContext::new();
    user_msg.set("role", "user");
    user_msg.set("content", text.as_str());
    msgs.push(user_msg);

    let mut ai_msg = TemplateContext::new();
    ai_msg.set("role", "assistant");
    ai_msg.set("content", response.as_str());
    msgs.push(ai_msg);

    ctx.set("messages", TemplateValue::List(msgs));

    let html = crepuscularity_web::render_template_to_html(&state.template, &ctx).unwrap_or_else(|e| {
        format!("<html><body><h1>Template error</h1><pre>{e}</pre></body></html>")
    });

    Ok(Html(html))
}

async fn send_ipc_prompt(socket_path: &PathBuf, text: &str) -> Result<String, String> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let stream = UnixStream::connect(socket_path).map_err(|e| format!("Cannot connect to IPC: {e}"))?;
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| format!("Clone error: {e}"))?);

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "prompt",
        "params": {"text": text},
    });
    let mut line = serde_json::to_string(&request).map_err(|e| format!("JSON error: {e}"))?;
    line.push('\n');
    let mut w = stream;
    w.write_all(line.as_bytes()).map_err(|e| format!("Write error: {e}"))?;

    // Read the response
    let mut resp_line = String::new();
    reader.read_line(&mut resp_line).map_err(|e| format!("Read error: {e}"))?;

    let resp: serde_json::Value =
        serde_json::from_str(&resp_line).map_err(|e| format!("Parse error: {e}"))?;
    Ok(resp.get("result").and_then(|r| r.as_str()).unwrap_or("ok").to_string())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();
    let socket_path = args
        .windows(2)
        .find(|w| w[0] == "--socket-path")
        .map(|w| PathBuf::from(&w[1]))
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            PathBuf::from(home).join(".telekinesis/telekinesis.sock")
        });

    let app_crepus = include_str!("../../app.crepus");

    let state = Arc::new(AppState {
        template: app_crepus.to_string(),
        socket_path,
    });

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/chat", get(chat_handler))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    tracing::info!("Telekinesis web UI listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
