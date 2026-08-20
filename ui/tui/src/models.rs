use rx4::{ModelInfo, ModelRegistry};

use crate::app::ConfiguredProvider;
use crate::provider_catalog;

pub(crate) const GPT_5_CONTEXT_WINDOW: usize = 1_050_000;

/// pi 0.83.0 `openai-codex.json` — models beyond rs_ai_oauth's
/// `CHATGPT_CODEX_MODELS`, completing the exact ChatGPT Codex catalog:
/// gpt-5.3-codex-spark, gpt-5.4, gpt-5.4-mini, gpt-5.5, gpt-5.6-luna,
/// gpt-5.6-sol, gpt-5.6-terra.
pub(crate) const PI_CODEX_GPT56: [&str; 3] = ["gpt-5.6-luna", "gpt-5.6-sol", "gpt-5.6-terra"];

/// pi 0.83.0 `openai.json` GPT-5.x family, injected for the API-key provider
/// and deduped against rx4's registry.
pub(crate) const PI_OPENAI_GPT5: [&str; 21] = [
    "gpt-5",
    "gpt-5-chat-latest",
    "gpt-5-mini",
    "gpt-5-nano",
    "gpt-5-pro",
    "gpt-5.1",
    "gpt-5.2",
    "gpt-5.2-chat-latest",
    "gpt-5.2-pro",
    "gpt-5.3-chat-latest",
    "gpt-5.3-codex",
    "gpt-5.3-codex-spark",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.4-nano",
    "gpt-5.4-pro",
    "gpt-5.5",
    "gpt-5.5-pro",
    "gpt-5.6-luna",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
];

pub(crate) fn context_window_for_model(model: &str) -> usize {
    // pi 0.83.0 context windows for models outside rx4's registry.
    if model.starts_with("gpt-5.5") || model.starts_with("gpt-5.6") {
        GPT_5_CONTEXT_WINDOW
    } else {
        match model {
            "gpt-5.4-pro" => GPT_5_CONTEXT_WINDOW,
            "gpt-5.4-nano" | "gpt-5-mini" | "gpt-5-nano" | "gpt-5-pro" | "gpt-5.1" | "gpt-5.2"
            | "gpt-5.2-pro" | "gpt-5.3-codex" => 400_000,
            "gpt-5-chat-latest"
            | "gpt-5.2-chat-latest"
            | "gpt-5.3-chat-latest"
            | "gpt-5.3-codex-spark" => 128_000,
            _ => 128_000,
        }
    }
}

pub(crate) fn host_model_info(provider: &str, id: &str) -> ModelInfo {
    let mut info = ModelInfo::new(provider, id, context_window_for_model(id), 8_192);
    info.supports_tools = true;
    info.supports_reasoning = id.contains("reason")
        || id.starts_with("o1")
        || id.starts_with("o3")
        || id.starts_with("gpt-5");
    info.supports_reasoning_effort = info.supports_reasoning;
    info
}

/// Build the metadata Rotary receives from this host's configured providers.
/// Dynamic provider snapshots extend this registry later; these entries only
/// keep the picker and compaction useful before the first network refresh.
pub(crate) fn initial_model_registry(providers: &[(ConfiguredProvider, String)]) -> ModelRegistry {
    let mut registry = ModelRegistry::new();
    for (provider, default_model) in providers {
        registry.register(host_model_info(&provider.id, default_model));
        if provider.id == "openai-codex" {
            for id in rs_ai_oauth::codex::CHATGPT_CODEX_MODELS
                .iter()
                .chain(PI_CODEX_GPT56.iter())
            {
                registry.register(host_model_info(&provider.id, id));
            }
        }
        if let Some(spec) = provider_catalog::by_id(&provider.id) {
            for id in spec.models {
                registry.register(host_model_info(&provider.id, id));
            }
        }
        if provider.id == "openai" {
            for id in PI_OPENAI_GPT5 {
                registry.register(host_model_info(&provider.id, id));
            }
        }
    }
    registry
}

pub(crate) fn oauth_model_info(provider: &str, model: rs_ai_oauth::ModelInfo) -> ModelInfo {
    let context_window = model
        .limits
        .context_window
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or_else(|| context_window_for_model(&model.id));
    let max_output_tokens = model
        .limits
        .max_output_tokens
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(8_192);
    let mut info = ModelInfo::new(provider, model.id, context_window, max_output_tokens);
    info.supports_tools = model.capabilities.contains("tool_calling")
        || model
            .supported_parameters
            .iter()
            .any(|parameter| matches!(parameter.as_str(), "tools" | "tool_choice"));
    info.supports_vision = model.capabilities.contains("image_input")
        || model
            .input_modalities
            .iter()
            .any(|modality| modality == "image");
    info.supports_reasoning = model.capabilities.contains("extended_thinking")
        || model
            .supported_parameters
            .iter()
            .any(|parameter| matches!(parameter.as_str(), "reasoning" | "include_reasoning"));
    info.supports_reasoning_effort = info.supports_reasoning;
    info
}

pub(crate) fn openrouter_model_info(value: &serde_json::Value) -> Option<ModelInfo> {
    let id = value.get("id")?.as_str()?;
    let context_window = value
        .get("top_provider")
        .and_then(|provider| provider.get("context_length"))
        .or_else(|| value.get("context_length"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(128_000);
    let max_output_tokens = value
        .get("top_provider")
        .and_then(|provider| provider.get("max_completion_tokens"))
        .or_else(|| value.get("max_output_tokens"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(8_192);
    let mut info = ModelInfo::new("openrouter", id, context_window, max_output_tokens);
    if let Some(parameters) = value
        .get("supported_parameters")
        .and_then(serde_json::Value::as_array)
    {
        info.supports_tools = parameters
            .iter()
            .any(|parameter| matches!(parameter.as_str(), Some("tools") | Some("tool_choice")));
        info.supports_reasoning = parameters.iter().any(|parameter| {
            matches!(
                parameter.as_str(),
                Some("reasoning") | Some("include_reasoning")
            )
        });
        info.supports_reasoning_effort = info.supports_reasoning;
    }
    info.supports_vision = value
        .get("architecture")
        .and_then(|architecture| architecture.get("input_modalities"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|modalities| {
            modalities
                .iter()
                .any(|modality| modality.as_str() == Some("image"))
        });
    Some(info)
}
