use std::sync::Arc;

use async_trait::async_trait;
use futures::stream;
use rs_ai_oauth::codex::{codex_request_body, ChatGptCodexClient};
use rx4::agent::ToolCall;
use rx4::provider::{Message, Provider, ProviderError, Role, StreamEvent, StreamResult};
use serde_json::{json, Value};

pub struct CodexProvider {
    client: ChatGptCodexClient,
}

impl CodexProvider {
    pub fn new(access_token: impl Into<String>) -> Self {
        Self {
            client: ChatGptCodexClient::new(access_token).with_originator("telekinesis"),
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
        let response = self
            .client
            .complete(body, None)
            .await
            .map_err(|error| ProviderError::Api(error.to_string()))?;
        let mut events = Vec::new();
        if !response.text.is_empty() {
            events.push(Ok(StreamEvent::Delta(response.text)));
        }
        events.extend(response.tool_calls.into_iter().map(|call| {
            Ok(StreamEvent::ToolCall(ToolCall {
                id: call.id,
                name: call.name,
                arguments: call.arguments,
            }))
        }));
        events.push(Ok(StreamEvent::Done));
        Ok(Box::new(stream::iter(events)))
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
}
