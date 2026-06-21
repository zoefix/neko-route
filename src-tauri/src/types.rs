use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    OfficialOpenAi,
    OfficialOpenAiAccount,
    OfficialAnthropicCli,
    OfficialAnthropicDesktop,
    OfficialAnthropicAccount,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProtocol {
    OpenAiResponses,
    OpenAiChatCompletions,
    AnthropicMessages,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodexInjectionMode {
    OfficialAccount,
    ThirdPartyApi,
    LanShare,
}

impl Default for CodexInjectionMode {
    fn default() -> Self {
        Self::OfficialAccount
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub kind: ProviderKind,
    pub protocol: ProviderProtocol,
    pub base_url: String,
    pub key_ref: Option<String>,
    #[serde(default)]
    pub http_proxy: ProviderHttpProxy,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderHttpProxy {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub context_window: u64,
    pub enabled: bool,
    pub provider_id: String,
    pub upstream_model: Option<String>,
    pub timeout_ms: u64,
    pub retry_count: u8,
    #[serde(default)]
    pub reasoning_enabled: bool,
    #[serde(default)]
    pub default_reasoning_level: String,
    #[serde(default)]
    pub supported_reasoning_levels: Vec<String>,
    #[serde(default)]
    pub codex_alias: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodexSlotAssignment {
    #[serde(default)]
    pub mode: CodexInjectionMode,
    #[serde(default)]
    pub source: String,
    pub slot: String,
    pub target_model_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub bind_host: String,
    pub port: u16,
    pub allow_lan: bool,
    #[serde(default = "default_lan_api_key")]
    pub lan_api_key: String,
    #[serde(default)]
    pub lan_remote_host: String,
    #[serde(default = "default_lan_remote_port")]
    pub lan_remote_port: u16,
    #[serde(default)]
    pub lan_remote_api_key: String,
    pub request_log_limit: usize,
    /// When set, any request for a model that is not configured is routed to
    /// this model instead of failing. Lets you confine Codex's auxiliary
    /// model calls (e.g. its "mini" helper) to a model you control.
    #[serde(default)]
    pub fallback_model: Option<String>,
    /// When true, the Codex config is re-injected automatically on every
    /// config change (toggling models, deleting providers, etc.).
    #[serde(default)]
    pub auto_inject: bool,
    /// Default model written into the Codex config during (auto-)injection.
    #[serde(default)]
    pub codex_default_model: Option<String>,
    /// How Codex authenticates when routed through Neko Route.
    #[serde(default)]
    pub codex_injection_mode: CodexInjectionMode,
    /// Keep Codex auxiliary/internal model slugs on the selected Codex default
    /// model instead of letting them hit similarly named upstream models.
    #[serde(default = "default_true")]
    pub codex_internal_model_lock: bool,
    #[serde(default)]
    pub codex_slots: Vec<CodexSlotAssignment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub version: u32,
    pub providers: Vec<Provider>,
    pub models: Vec<ModelEntry>,
    pub settings: Settings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyStatus {
    pub provider_id: String,
    pub present: bool,
    pub available: bool,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerStatus {
    pub bind_url: String,
    pub running: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
}

impl TokenUsage {
    pub fn is_empty(&self) -> bool {
        self.input_tokens == 0
            && self.output_tokens == 0
            && self.cache_read_tokens == 0
            && self.cache_write_tokens == 0
            && self.total_tokens == 0
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextBridgeDiagnostics {
    #[serde(default)]
    pub strategy: String,
    #[serde(default)]
    pub original_body_bytes: u64,
    #[serde(default)]
    pub final_body_bytes: u64,
    #[serde(default)]
    pub original_tool_result_bytes: u64,
    #[serde(default)]
    pub tool_result_count: u64,
    #[serde(default)]
    pub kept_tool_results: u64,
    #[serde(default)]
    pub archived_tool_results: u64,
    #[serde(default)]
    pub archived_bytes: u64,
    #[serde(default)]
    pub recalled_artifacts: u64,
    #[serde(default)]
    pub recalled_bytes: u64,
    #[serde(default)]
    pub count_tokens_input_tokens: Option<u64>,
    #[serde(default)]
    pub count_tokens_error: Option<String>,
    #[serde(default)]
    pub context_management: bool,
    #[serde(default)]
    pub raw_precheck_input_tokens: Option<u64>,
    #[serde(default)]
    pub final_input_tokens: Option<u64>,
    #[serde(default)]
    pub estimated_input_tokens: Option<u64>,
    #[serde(default)]
    pub estimate_source: Option<String>,
    #[serde(default)]
    pub estimate_confidence: Option<String>,
    #[serde(default)]
    pub protection_triggered: bool,
    #[serde(default)]
    pub target_input_tokens: Option<u64>,
    #[serde(default)]
    pub previous_success_input_tokens: Option<u64>,
    #[serde(default)]
    pub previous_success_body_bytes: Option<u64>,
    #[serde(default)]
    pub compression_stage: Option<String>,
    #[serde(default)]
    pub protection_failure_reason: Option<String>,
    #[serde(default)]
    pub compression_reason: Option<String>,
    #[serde(default)]
    pub last_message_role: Option<String>,
    #[serde(default)]
    pub last_message_content_type: Option<String>,
    #[serde(default)]
    pub last_message_text_length: u64,
    #[serde(default)]
    pub last_message_preview_head: Option<String>,
    #[serde(default)]
    pub last_message_preview_tail: Option<String>,
    #[serde(default)]
    pub last_message_from_function_call_output: bool,
    #[serde(default)]
    pub single_dot_user_message: bool,
    #[serde(default)]
    pub latest_tool_result_count: u64,
    #[serde(default)]
    pub latest_tool_result_text_length: u64,
    #[serde(default)]
    pub latest_tool_result_single_dot: bool,
}

#[derive(Debug, Clone)]
pub struct ClaudeContextPressureSample {
    pub input_tokens: u64,
    pub body_bytes: u64,
    pub requires_precompression: bool,
    pub context_full_body_bytes: u64,
    pub compression_stage: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ContextArtifact {
    pub hash: String,
    pub created_at: DateTime<Utc>,
    pub request_id: String,
    pub model: String,
    pub tool_name: Option<String>,
    pub tool_args: Option<String>,
    pub content_bytes: u64,
    pub content_text: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestRecord {
    pub id: String,
    pub started_at: DateTime<Utc>,
    pub model: String,
    #[serde(default)]
    pub requested_model: Option<String>,
    #[serde(default)]
    pub route_reason: Option<String>,
    pub provider_id: Option<String>,
    pub provider_name: Option<String>,
    pub provider_protocol: Option<ProviderProtocol>,
    pub status: u16,
    pub latency_ms: u128,
    pub streaming: bool,
    pub error: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub stream_state: Option<String>,
    #[serde(default)]
    pub stream_error: Option<String>,
    #[serde(default)]
    pub last_event: Option<String>,
    #[serde(default)]
    pub stream_bytes: u64,
    #[serde(default)]
    pub context_bridge: Option<ContextBridgeDiagnostics>,
    #[serde(default)]
    pub usage: TokenUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLogPage {
    pub records: Vec<RequestRecord>,
    pub total: u64,
    pub page: usize,
    pub page_size: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
    pub requests: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DayTokens {
    pub date: String,
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub requests: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelTokens {
    pub model: String,
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub requests: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenStats {
    pub today: TokenTotals,
    pub yesterday: TokenTotals,
    pub last7: TokenTotals,
    pub all_time: TokenTotals,
    pub series: Vec<DayTokens>,
    pub by_model: Vec<ModelTokens>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderLocalUsage {
    pub provider_id: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
    pub requests: u64,
    pub estimated_cost_usd: Option<f64>,
    pub unknown_cost_models: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfficialQuotaWindow {
    pub used_percent: f64,
    pub limit_window_seconds: u64,
    pub reset_after_seconds: u64,
    pub reset_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OfficialAccountQuota {
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub plan_label: Option<String>,
    pub subscription_expires_at: Option<DateTime<Utc>>,
    pub five_hour: Option<OfficialQuotaWindow>,
    pub seven_day: Option<OfficialQuotaWindow>,
    pub reset_credits: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderUsageStatus {
    pub provider_id: String,
    pub quota: Option<OfficialAccountQuota>,
    pub local_usage: ProviderLocalUsage,
    pub updated_at: Option<DateTime<Utc>>,
    pub source: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSnapshot {
    pub config: AppConfig,
    pub keys: Vec<KeyStatus>,
    pub server: ServerStatus,
    #[serde(default)]
    pub codex_apply_error: Option<String>,
    pub requests: Vec<RequestRecord>,
    pub request_log_count: u64,
    pub stats: TokenStats,
    pub provider_usage: Vec<ProviderUsageStatus>,
    pub codex_home: String,
}

pub fn default_config() -> AppConfig {
    AppConfig {
        version: 13,
        providers: vec![
            Provider {
                id: "openai-official".into(),
                name: "OpenAI Official Account".into(),
                kind: ProviderKind::OfficialOpenAi,
                protocol: ProviderProtocol::OpenAiResponses,
                base_url: "https://api.openai.com/v1".into(),
                key_ref: None,
                http_proxy: ProviderHttpProxy::default(),
            },
            Provider {
                id: "anthropic-cli".into(),
                name: "Claude Code CLI Official".into(),
                kind: ProviderKind::OfficialAnthropicCli,
                protocol: ProviderProtocol::AnthropicMessages,
                base_url: "local://claude-code".into(),
                key_ref: None,
                http_proxy: ProviderHttpProxy::default(),
            },
            Provider {
                id: "anthropic-desktop".into(),
                name: "Claude Desktop Official".into(),
                kind: ProviderKind::OfficialAnthropicDesktop,
                protocol: ProviderProtocol::AnthropicMessages,
                base_url: "local://claude-desktop".into(),
                key_ref: None,
                http_proxy: ProviderHttpProxy::default(),
            },
        ],
        models: vec![
            model(
                "gpt-5.5",
                "GPT-5.5",
                "OpenAI",
                1_000_000,
                "openai-official",
                ProviderProtocol::OpenAiResponses,
            ),
            model(
                "claude-opus-4-8",
                "Claude Opus 4.8",
                "Claude CLI",
                200_000,
                "anthropic-cli",
                ProviderProtocol::AnthropicMessages,
            ),
            model(
                "claude-sonnet-4-5",
                "Claude Sonnet 4.5",
                "Claude Desktop",
                200_000,
                "anthropic-desktop",
                ProviderProtocol::AnthropicMessages,
            ),
        ],
        settings: Settings {
            bind_host: "127.0.0.1".into(),
            port: 8787,
            allow_lan: false,
            lan_api_key: default_lan_api_key(),
            lan_remote_host: String::new(),
            lan_remote_port: default_lan_remote_port(),
            lan_remote_api_key: String::new(),
            request_log_limit: 300,
            fallback_model: Some("gpt-5.5".into()),
            auto_inject: false,
            codex_default_model: None,
            codex_injection_mode: CodexInjectionMode::OfficialAccount,
            codex_internal_model_lock: true,
            codex_slots: Vec::new(),
        },
    }
}

fn default_true() -> bool {
    true
}

fn default_lan_remote_port() -> u16 {
    8787
}

pub fn default_lan_api_key() -> String {
    format!("nr_{}", uuid::Uuid::new_v4().simple())
}

fn model(
    id: &str,
    display_name: &str,
    description: &str,
    context_window: u64,
    provider_id: &str,
    protocol: ProviderProtocol,
) -> ModelEntry {
    let (reasoning_enabled, default_reasoning_level, supported_reasoning_levels) =
        reasoning_defaults_for_protocol(&protocol);
    ModelEntry {
        id: id.into(),
        display_name: display_name.into(),
        description: description.into(),
        context_window,
        enabled: true,
        provider_id: provider_id.into(),
        upstream_model: None,
        timeout_ms: default_timeout_ms(),
        retry_count: default_retry_count(),
        reasoning_enabled,
        default_reasoning_level,
        supported_reasoning_levels,
        codex_alias: None,
    }
}

pub fn reasoning_defaults_for_protocol(protocol: &ProviderProtocol) -> (bool, String, Vec<String>) {
    match protocol {
        ProviderProtocol::AnthropicMessages => (
            true,
            "max".into(),
            reasoning_levels(&["low", "medium", "high", "xhigh", "max"]),
        ),
        ProviderProtocol::OpenAiResponses => (
            true,
            "xhigh".into(),
            reasoning_levels(&["low", "medium", "high", "xhigh"]),
        ),
        ProviderProtocol::OpenAiChatCompletions => (
            true,
            "xhigh".into(),
            reasoning_levels(&["low", "medium", "high", "xhigh"]),
        ),
    }
}

fn reasoning_levels(levels: &[&str]) -> Vec<String> {
    levels.iter().map(|level| (*level).to_string()).collect()
}

fn default_timeout_ms() -> u64 {
    0
}

fn default_retry_count() -> u8 {
    0
}
