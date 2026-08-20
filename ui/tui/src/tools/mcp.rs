use std::sync::Arc;

use rx4::agent::{ToolDefinition, ToolEffect, ToolResult};
use rx4::ToolRegistry;

use crate::mcp_config;

pub(crate) struct McpToolSpec {
    pub(crate) full_name: String,
    description: String,
    parameters: String,
    remote_name: String,
    client: Arc<rx4::McpClient>,
}

const MCP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub(crate) async fn discover_mcp_tools() -> (Vec<McpToolSpec>, Vec<String>) {
    let configs = mcp_config::load();
    let mut specs = Vec::new();
    let mut errors = Vec::new();
    if configs.is_empty() {
        return (specs, errors);
    }

    let results = futures::future::join_all(configs.into_iter().map(|cfg| {
        let name = cfg.name.clone();
        async move {
            let listed = tokio::time::timeout(MCP_CONNECT_TIMEOUT, async {
                let client = rx4::McpClient::connect_config(&cfg).await?;
                let listed = client.list_tools().await?;
                Ok::<_, anyhow::Error>((Arc::new(client), listed))
            })
            .await;
            (name, cfg, listed)
        }
    }))
    .await;

    for (name, cfg, listed) in results {
        let (client, listed) = match listed {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => {
                errors.push(format!("MCP server `{name}` unavailable: {e}"));
                continue;
            }
            Err(_) => {
                errors.push(format!(
                    "MCP server `{name}` timed out after {}s",
                    MCP_CONNECT_TIMEOUT.as_secs()
                ));
                continue;
            }
        };
        for tool in listed {
            let description = if tool.description.is_empty() {
                format!("MCP tool {} from {}", tool.name, cfg.name)
            } else {
                tool.description.clone()
            };
            specs.push(McpToolSpec {
                full_name: format!("mcp__{}__{}", cfg.name, tool.name),
                description,
                parameters: tool.input_schema.to_string(),
                remote_name: tool.name.clone(),
                client: client.clone(),
            });
        }
    }
    (specs, errors)
}

pub(crate) fn register_mcp_tools(tools: &mut ToolRegistry, specs: &[McpToolSpec]) {
    for spec in specs {
        let client = spec.client.clone();
        let remote_name = spec.remote_name.clone();
        tools.register(
            ToolDefinition::new_boxed(
                spec.full_name.clone(),
                spec.description.clone(),
                spec.parameters.clone(),
                Box::new(move |_ctx, args| {
                    let client = client.clone();
                    let remote_name = remote_name.clone();
                    Box::pin(async move {
                        let value: serde_json::Value = serde_json::from_str(&args)
                            .unwrap_or_else(|_| serde_json::json!({ "raw": args }));
                        match client.call_tool(&remote_name, &value).await {
                            Ok(v) => ToolResult::ok(remote_name.clone(), v.to_string()),
                            Err(e) => ToolResult::err(remote_name.clone(), e.to_string()),
                        }
                    })
                }),
            )
            .with_effect(ToolEffect::Network),
        );
    }
}
