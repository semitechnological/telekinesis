use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream;
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use rs_ai_oauth::codex::{codex_request_body, ChatGptCodexClient};
use rx4::agent::ToolCall;
use rx4::provider::{Message, Provider, ProviderError, Role, StreamEvent, StreamResult};
use serde_json::{json, Value};

pub struct CodexProvider {
    client: ChatGptCodexClient,
    token: String,
}

impl CodexProvider {
    pub fn new(access_token: impl Into<String>) -> Self {
        let token = access_token.into();
        Self {
            client: ChatGptCodexClient::new(token.clone()).with_originator("telekinesis-gui"),
            token,
        }
    }
}

#[async_trait]
impl Provider for CodexProvider {
    fn id(&self) -> &str {
        "openai-codex"
    }

    fn name(&self) -> &str {
        "ChatGPT Codex"
    }

    async fn stream(
        &self,
        messages: &[Message],
        system: &Option<String>,
        model: &str,
        tools: &[Value],
        reasoning_effort: Option<&str>,
    ) -> Result<StreamResult, ProviderError> {
        let input = messages_to_responses_input(messages);
        let tools = tools_to_responses_tools(tools);
        let body = codex_request_body(
            model,
            system.as_deref().unwrap_or("You are a helpful assistant."),
            input,
            tools,
            reasoning_effort,
        );
        let client = reqwest::Client::new();
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", self.token))
                .map_err(|error| ProviderError::Api(error.to_string()))?,
        );
        headers.insert("originator", HeaderValue::from_static("telekinesis-gui"));
        headers.insert(USER_AGENT, HeaderValue::from_static("telekinesis-gui"));
        headers.insert(
            "openai-beta",
            HeaderValue::from_static("responses=experimental"),
        );
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(account_id) = self.client.account_id() {
            headers.insert(
                "chatgpt-account-id",
                HeaderValue::from_str(account_id)
                    .map_err(|error| ProviderError::Api(error.to_string()))?,
            );
        }

        let response = client
            .post("https://chatgpt.com/backend-api/codex/responses")
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                ProviderError::Api(format!("ChatGPT Codex request failed: {error}"))
            })?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Api(format!(
                "ChatGPT Codex request failed (HTTP {status}): {}",
                text.chars().take(300).collect::<String>()
            )));
        }

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<StreamEvent, ProviderError>>();
        let mut byte_stream = response.bytes_stream();
        tokio::spawn(async move {
            let mut buffer = String::new();
            let mut calls = BTreeMap::new();
            let mut failed = None;
            while let Some(chunk) = byte_stream.next().await {
                match chunk {
                    Ok(chunk) => buffer.push_str(&String::from_utf8_lossy(&chunk)),
                    Err(error) => {
                        failed = Some(ProviderError::Api(format!(
                            "codex stream read failed: {error}"
                        )));
                        break;
                    }
                }
                while let Some(position) = buffer.find("\n\n") {
                    let block = buffer[..position].to_string();
                    buffer = buffer[position + 2..].to_string();
                    if let Err(error) = handle_sse_block(&block, &tx, &mut calls) {
                        failed = Some(error);
                        break;
                    }
                }
                if failed.is_some() {
                    break;
                }
            }
            if failed.is_none() && !buffer.trim().is_empty() {
                failed = handle_sse_block(&buffer, &tx, &mut calls).err();
            }
            if let Some(error) = failed {
                let _ = tx.send(Err(error));
            } else {
                for call in calls.into_values() {
                    let _ = tx.send(Ok(StreamEvent::ToolCall(call)));
                }
            }
            let _ = tx.send(Ok(StreamEvent::Done));
        });

        let stream = stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(Box::new(Box::pin(stream)))
    }
}

fn handle_sse_block(
    block: &str,
    tx: &tokio::sync::mpsc::UnboundedSender<Result<StreamEvent, ProviderError>>,
    calls: &mut BTreeMap<usize, ToolCall>,
) -> Result<(), ProviderError> {
    let data = block
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n");
    if data.is_empty() || data == "[DONE]" {
        return Ok(());
    }
    let event: Value = serde_json::from_str(&data)
        .map_err(|error| ProviderError::Api(format!("invalid ChatGPT Codex SSE event: {error}")))?;
    let event_type = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match event_type {
        "response.output_text.delta" | "response.reasoning_summary_text.delta" => {
            let delta = event
                .get("delta")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !delta.is_empty() {
                let _ = tx.send(Ok(StreamEvent::Delta(delta.to_string())));
            }
        }
        "response.output_item.added" | "response.output_item.done" => {
            let item = event.get("item").unwrap_or(&Value::Null);
            if item.get("type").and_then(Value::as_str) == Some("function_call") {
                let index = event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(calls.len() as u64) as usize;
                let call = calls.entry(index).or_insert_with(empty_tool_call);
                if let Some(value) = item.get("call_id").and_then(Value::as_str) {
                    call.id = value.to_string();
                }
                if let Some(value) = item.get("name").and_then(Value::as_str) {
                    call.name = value.to_string();
                }
                if let Some(value) = item.get("arguments").and_then(Value::as_str) {
                    call.arguments = value.to_string();
                }
            }
        }
        "response.function_call_arguments.delta" => {
            let index = event
                .get("output_index")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            let call = calls.entry(index).or_insert_with(empty_tool_call);
            call.arguments.push_str(
                event
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            );
        }
        "response.function_call_arguments.done" => {
            let index = event
                .get("output_index")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            let call = calls.entry(index).or_insert_with(empty_tool_call);
            call.arguments = event
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
        }
        "error" | "response.failed" => {
            return Err(ProviderError::Api(format!(
                "ChatGPT Codex error: {}",
                event.to_string().chars().take(300).collect::<String>()
            )));
        }
        _ => {}
    }
    Ok(())
}

fn empty_tool_call() -> ToolCall {
    ToolCall {
        id: String::new(),
        name: String::new(),
        arguments: String::new(),
    }
}

fn messages_to_responses_input(messages: &[Message]) -> Vec<Value> {
    let mut input = Vec::new();
    for message in messages {
        match message.role {
            Role::System => {}
            Role::User => input.push(json!({
                "role": "user",
                "content": [{"type": "input_text", "text": message.content}],
            })),
            Role::Assistant => {
                if !message.content.is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": message.content, "annotations": []}],
                        "status": "completed",
                    }));
                }
                for call in &message.tool_calls {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call.id,
                        "name": call.name,
                        "arguments": call.arguments,
                    }));
                }
            }
            Role::Tool => input.push(json!({
                "type": "function_call_output",
                "call_id": message.tool_call_id.clone().unwrap_or_default(),
                "output": message.content,
            })),
        }
    }
    input
}

fn tools_to_responses_tools(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            let function = tool.get("function").unwrap_or(tool);
            json!({
                "type": "function",
                "name": function.get("name").cloned().unwrap_or(Value::Null),
                "description": function.get("description").cloned().unwrap_or(Value::Null),
                "parameters": function.get("parameters").cloned().unwrap_or_else(|| json!({})),
                "strict": null,
            })
        })
        .collect()
}

pub fn provider_arc(access_token: impl Into<String>) -> Arc<dyn Provider> {
    Arc::new(CodexProvider::new(access_token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_tool_messages_to_responses_items() {
        let messages = vec![
            Message::user("hello"),
            Message::assistant_with_tools(
                "",
                vec![ToolCall {
                    id: "call_1".into(),
                    name: "shell".into(),
                    arguments: "{}".into(),
                }],
            ),
            Message::tool("call_1", "ok"),
        ];
        let input = messages_to_responses_input(&messages);
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[2]["type"], "function_call_output");
    }

    #[test]
    fn converts_openai_tools_to_responses_tools() {
        let tools = vec![json!({
            "type": "function",
            "function": {
                "name": "shell",
                "description": "run a command",
                "parameters": {"type": "object"}
            }
        })];
        let converted = tools_to_responses_tools(&tools);
        assert_eq!(converted[0]["type"], "function");
        assert_eq!(converted[0]["name"], "shell");
    }

    #[test]
    fn streams_text_delta() {
        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<Result<StreamEvent, ProviderError>>();
        let mut calls = BTreeMap::new();
        handle_sse_block(
            r#"data: {"type":"response.output_text.delta","delta":"hello"}"#,
            &tx,
            &mut calls,
        )
        .expect("text delta should parse");
        match rx.try_recv().expect("text delta should be emitted") {
            Ok(StreamEvent::Delta(delta)) => assert_eq!(delta, "hello"),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn collects_streamed_tool_arguments() {
        let (tx, _rx) =
            tokio::sync::mpsc::unbounded_channel::<Result<StreamEvent, ProviderError>>();
        let mut calls = BTreeMap::new();
        handle_sse_block(
            r#"data: {"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"call_1","name":"shell","arguments":""}}"#,
            &tx,
            &mut calls,
        )
        .expect("tool call should parse");
        handle_sse_block(
            r#"data: {"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"cmd\":"}"#,
            &tx,
            &mut calls,
        )
        .expect("tool delta should parse");
        handle_sse_block(
            r#"data: {"type":"response.function_call_arguments.done","output_index":0,"arguments":"{\"cmd\":\"pwd\"}"}"#,
            &tx,
            &mut calls,
        )
        .expect("tool arguments should parse");
        let call = calls.get(&0).expect("tool call should be collected");
        assert_eq!(call.id, "call_1");
        assert_eq!(call.name, "shell");
        assert_eq!(call.arguments, r#"{"cmd":"pwd"}"#);
    }
}
