//! Host-owned provider catalog for the `/providers` surface.
//!
//! Every API-key entry below uses an endpoint that rx4's OpenAI-compatible
//! provider can drive. Providers which need a bespoke protocol (Bedrock,
//! Azure Responses, Vertex ADC, etc.) remain engine work rather than being
//! advertised as working configurations here.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderApi {
    OpenAiCompatible,
    Anthropic,
}

#[derive(Clone, Copy, Debug)]
pub struct ProviderSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub env: &'static str,
    pub base_url: &'static str,
    pub api: ProviderApi,
    pub default_model: &'static str,
    pub models: &'static [&'static str],
    pub aliases: &'static [&'static str],
}

const CLAUDE: &[&str] = &["claude-sonnet-4-5", "claude-opus-4-6", "claude-haiku-4-5"];
const OPENAI: &[&str] = &["gpt-5.4", "gpt-5.4-mini", "gpt-5.6-sol"];
const XAI: &[&str] = &["grok-4.5", "grok-4.3", "grok-3-mini"];
const GEMINI: &[&str] = &["gemini-2.5-pro", "gemini-2.5-flash", "gemini-2.0-flash"];
const OPENCODE_GO: &[&str] = &[
    "deepseek-v4-flash",
    "deepseek-v4-pro",
    "glm-5.1",
    "glm-5.2",
    "kimi-k2.6",
    "kimi-k2.7-code",
    "mimo-v2.5",
];
const ZAI: &[&str] = &["glm-4.7", "glm-5.1", "glm-5.2"];
const MIMO: &[&str] = &[
    "mimo-v2-omni",
    "mimo-v2-pro",
    "mimo-v2.5",
    "mimo-v2.5-pro",
    "mimo-v2.5-pro-ultraspeed",
];
const DEEPSEEK: &[&str] = &["deepseek-chat", "deepseek-reasoner"];
const ROUTER: &[&str] = &[
    "openrouter/auto",
    "anthropic/claude-sonnet-4-5",
    "openai/gpt-5.4",
];
const GROQ: &[&str] = &["llama-3.3-70b-versatile", "qwen/qwen3-32b"];
const CEREBRAS: &[&str] = &["zai-glm-4.7", "llama-3.3-70b"];
const TOGETHER: &[&str] = &[
    "meta-llama/Llama-3.3-70B-Instruct-Turbo",
    "Qwen/Qwen3-Coder-480B-A35B-Instruct-FP8",
];
const MISTRAL: &[&str] = &["mistral-large-latest", "codestral-latest"];
const NVIDIA: &[&str] = &["meta/llama-3.3-70b-instruct", "deepseek-ai/deepseek-r1"];
const FIREWORKS: &[&str] = &[
    "accounts/fireworks/models/deepseek-v3p1",
    "accounts/fireworks/models/llama-v3p3-70b-instruct",
];
const HUGGINGFACE: &[&str] = &["Qwen/Qwen3-Coder-Next", "deepseek-ai/DeepSeek-V3.2"];

/// API-key providers directly supported by the current rx4 provider API.
///
/// This is intentionally a compact, curated subset of Pi/OpenCode's much
/// broader catalog: it contains every provider this host can truthfully drive
/// without faking a protocol implementation.
pub const API_KEY_PROVIDERS: &[ProviderSpec] = &[
    ProviderSpec {
        id: "openai",
        name: "OpenAI",
        env: "OPENAI_API_KEY",
        base_url: "https://api.openai.com/v1",
        api: ProviderApi::OpenAiCompatible,
        default_model: "gpt-5.4",
        models: OPENAI,
        aliases: &["gpt"],
    },
    ProviderSpec {
        id: "xai",
        name: "xAI",
        env: "XAI_API_KEY",
        base_url: "https://api.x.ai/v1",
        api: ProviderApi::OpenAiCompatible,
        default_model: "grok-4.5",
        models: XAI,
        aliases: &["grok"],
    },
    ProviderSpec {
        id: "google",
        name: "Google Gemini",
        env: "GOOGLE_API_KEY",
        base_url: "https://generativelanguage.googleapis.com/v1beta",
        api: ProviderApi::OpenAiCompatible,
        default_model: "gemini-2.0-flash",
        models: GEMINI,
        aliases: &["gemini"],
    },
    ProviderSpec {
        id: "anthropic",
        name: "Anthropic",
        env: "ANTHROPIC_API_KEY",
        base_url: "https://api.anthropic.com/v1",
        api: ProviderApi::Anthropic,
        default_model: "claude-sonnet-4-5",
        models: CLAUDE,
        aliases: &["claude"],
    },
    ProviderSpec {
        id: "openrouter",
        name: "OpenRouter",
        env: "OPENROUTER_API_KEY",
        base_url: "https://openrouter.ai/api/v1",
        api: ProviderApi::OpenAiCompatible,
        default_model: "openrouter/auto",
        models: ROUTER,
        aliases: &["router"],
    },
    ProviderSpec {
        id: "opencode-go",
        name: "OpenCode Zen Go",
        env: "OPENCODE_API_KEY",
        base_url: "https://opencode.ai/zen/go/v1",
        api: ProviderApi::OpenAiCompatible,
        default_model: "deepseek-v4-flash",
        models: OPENCODE_GO,
        aliases: &["opencode", "go", "zen"],
    },
    ProviderSpec {
        id: "deepseek",
        name: "DeepSeek",
        env: "DEEPSEEK_API_KEY",
        base_url: "https://api.deepseek.com",
        api: ProviderApi::OpenAiCompatible,
        default_model: "deepseek-chat",
        models: DEEPSEEK,
        aliases: &[],
    },
    ProviderSpec {
        id: "groq",
        name: "Groq",
        env: "GROQ_API_KEY",
        base_url: "https://api.groq.com/openai/v1",
        api: ProviderApi::OpenAiCompatible,
        default_model: "llama-3.3-70b-versatile",
        models: GROQ,
        aliases: &[],
    },
    ProviderSpec {
        id: "cerebras",
        name: "Cerebras",
        env: "CEREBRAS_API_KEY",
        base_url: "https://api.cerebras.ai/v1",
        api: ProviderApi::OpenAiCompatible,
        default_model: "zai-glm-4.7",
        models: CEREBRAS,
        aliases: &[],
    },
    ProviderSpec {
        id: "together",
        name: "Together AI",
        env: "TOGETHER_API_KEY",
        base_url: "https://api.together.ai/v1",
        api: ProviderApi::OpenAiCompatible,
        default_model: "meta-llama/Llama-3.3-70B-Instruct-Turbo",
        models: TOGETHER,
        aliases: &["togetherai"],
    },
    ProviderSpec {
        id: "mistral",
        name: "Mistral",
        env: "MISTRAL_API_KEY",
        base_url: "https://api.mistral.ai/v1",
        api: ProviderApi::OpenAiCompatible,
        default_model: "mistral-large-latest",
        models: MISTRAL,
        aliases: &[],
    },
    ProviderSpec {
        id: "fireworks",
        name: "Fireworks AI",
        env: "FIREWORKS_API_KEY",
        base_url: "https://api.fireworks.ai/inference/v1",
        api: ProviderApi::OpenAiCompatible,
        default_model: "accounts/fireworks/models/deepseek-v3p1",
        models: FIREWORKS,
        aliases: &[],
    },
    ProviderSpec {
        id: "nvidia",
        name: "NVIDIA NIM",
        env: "NVIDIA_API_KEY",
        base_url: "https://integrate.api.nvidia.com/v1",
        api: ProviderApi::OpenAiCompatible,
        default_model: "meta/llama-3.3-70b-instruct",
        models: NVIDIA,
        aliases: &["nim"],
    },
    ProviderSpec {
        id: "huggingface",
        name: "Hugging Face",
        env: "HF_TOKEN",
        base_url: "https://router.huggingface.co/v1",
        api: ProviderApi::OpenAiCompatible,
        default_model: "Qwen/Qwen3-Coder-Next",
        models: HUGGINGFACE,
        aliases: &["hf"],
    },
    ProviderSpec {
        id: "zai",
        name: "Z.AI Coding Plan",
        env: "ZAI_API_KEY",
        base_url: "https://api.z.ai/api/coding/paas/v4",
        api: ProviderApi::OpenAiCompatible,
        default_model: "glm-5.2",
        models: ZAI,
        aliases: &["z.ai", "glm"],
    },
    ProviderSpec {
        id: "zai-coding-cn",
        name: "Z.AI Coding Plan CN",
        env: "ZAI_CODING_CN_API_KEY",
        base_url: "https://open.bigmodel.cn/api/coding/paas/v4",
        api: ProviderApi::OpenAiCompatible,
        default_model: "glm-5.2",
        models: ZAI,
        aliases: &["zai-cn"],
    },
    ProviderSpec {
        id: "xiaomi",
        name: "Xiaomi MiMo",
        env: "XIAOMI_API_KEY",
        base_url: "https://api.xiaomimimo.com/v1",
        api: ProviderApi::OpenAiCompatible,
        default_model: "mimo-v2.5",
        models: MIMO,
        aliases: &["mimo"],
    },
    ProviderSpec {
        id: "xiaomi-token-plan-cn",
        name: "Xiaomi Token Plan CN",
        env: "XIAOMI_TOKEN_PLAN_CN_API_KEY",
        base_url: "https://token-plan-cn.xiaomimimo.com/v1",
        api: ProviderApi::OpenAiCompatible,
        default_model: "mimo-v2.5",
        models: MIMO,
        aliases: &["mimo-cn"],
    },
    ProviderSpec {
        id: "xiaomi-token-plan-ams",
        name: "Xiaomi Token Plan AMS",
        env: "XIAOMI_TOKEN_PLAN_AMS_API_KEY",
        base_url: "https://token-plan-ams.xiaomimimo.com/v1",
        api: ProviderApi::OpenAiCompatible,
        default_model: "mimo-v2.5",
        models: MIMO,
        aliases: &["mimo-ams"],
    },
    ProviderSpec {
        id: "xiaomi-token-plan-sgp",
        name: "Xiaomi Token Plan SGP",
        env: "XIAOMI_TOKEN_PLAN_SGP_API_KEY",
        base_url: "https://token-plan-sgp.xiaomimimo.com/v1",
        api: ProviderApi::OpenAiCompatible,
        default_model: "mimo-v2.5",
        models: MIMO,
        aliases: &["mimo-sgp"],
    },
];

pub fn find(query: &str) -> Option<&'static ProviderSpec> {
    let query = query.trim().to_ascii_lowercase();
    API_KEY_PROVIDERS.iter().find(|provider| {
        provider.id == query
            || provider.name.to_ascii_lowercase() == query
            || provider.aliases.iter().any(|alias| *alias == query)
    })
}

pub fn by_id(id: &str) -> Option<&'static ProviderSpec> {
    API_KEY_PROVIDERS.iter().find(|provider| provider.id == id)
}

pub fn env_key(spec: &ProviderSpec) -> Option<String> {
    std::env::var(spec.env)
        .ok()
        .filter(|key| !key.trim().is_empty())
}
