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
    OpenAiImages,
    GeminiImage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodexInjectionMode {
    OfficialAccount,
    ThirdPartyApi,
    LanShare,
    DirectProvider,
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
    /// 是否图片生成模型(由 OpenAI Images 协议自动识别)。Codex 配置的「image_gen 模型」下拉据此过滤。
    #[serde(default)]
    pub image_generation: bool,
    /// 图片质量(low/medium/high)。仅图片模型用，转发 /v1/images 时若请求没传则注入。
    #[serde(default)]
    pub image_quality: Option<String>,
    /// 普通文本模型(OpenAI Responses/官方账号/官方客户端)是否也支持图片生成。
    /// 开启后该模型可被选为「图片生成模型」，画图走 /v1/images。
    #[serde(default)]
    pub image_capable: bool,
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
    /// 当 Codex 触发画图(image_gen 技能)时，强制把请求路由到这个图片模型，
    /// 覆盖默认路由。None = 默认，不改变路由。三种应用模式(官方/第三方/局域网)都生效。
    #[serde(default)]
    pub image_gen_model: Option<String>,
    /// 精确检测为「内部辅助请求」时强制路由到该模型(None=不覆盖)。要求 1M 上下文窗口。
    #[serde(default)]
    pub aux_model: Option<String>,
    /// 精确检测为「记忆写入 agent」时强制路由到该模型(None=不覆盖)。要求 1M 上下文窗口。
    #[serde(default)]
    pub memory_model: Option<String>,
    /// 直连模式选定的上游服务商 id；Codex 请求透传到该 provider，不做模型重定向。
    #[serde(default)]
    pub direct_provider_id: Option<String>,
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
    pub original_body_bytes: u64,
    #[serde(default)]
    pub final_body_bytes: u64,
    #[serde(default)]
    pub original_tool_result_bytes: u64,
    #[serde(default)]
    pub tool_result_count: u64,
    #[serde(default)]
    pub context_management: bool,
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
    #[serde(default)]
    pub tool_results_truncated: u64,
    #[serde(default)]
    pub tool_results_truncated_bytes: u64,
    #[serde(default)]
    pub context_management_edits: Option<String>,
    #[serde(default)]
    pub applied_edits: Option<String>,
    #[serde(default)]
    pub compaction_persisted: bool,
    #[serde(default)]
    pub compaction_injected: bool,
    // === OpenAI Responses 请求画像（仅 Responses 协议填充，用于日志识别请求用途）===
    /// 粗分类："main_coding"（主编码对话）| "auxiliary"（内部辅助请求）。
    #[serde(default)]
    pub request_kind: Option<String>,
    /// instructions 开头摘要。
    #[serde(default)]
    pub instructions_preview: Option<String>,
    /// instructions 总字符数（分类信号 + 展示）。
    #[serde(default)]
    pub instructions_length: u64,
    /// 工具数量。
    #[serde(default)]
    pub tool_count: u64,
    /// 前若干个工具名。
    #[serde(default)]
    pub tool_names: Vec<String>,
    /// input 消息条数。
    #[serde(default)]
    pub input_message_count: u64,
    /// 请求声明的最大输出 token。
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ClaudeContextPressureSample {
    pub compaction_summary: Option<String>,
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
    /// 清理前的真实上下文体积（给 Codex 判断占用 + UI TOKEN 列）。
    #[serde(default)]
    pub context_usage: TokenUsage,
    /// 路由命中的真实上游模型名；model 是随机路由 id 查不到市场定价，估价优先用它。
    #[serde(default)]
    pub upstream_model: Option<String>,
    /// 按上游模型市场定价估算的等效消费金额（基于清理后 usage）。
    #[serde(default)]
    pub cost_usd: Option<f64>,
    /// 画图请求生成的原图文件名（image_cache 内），日志可点击预览。
    #[serde(default)]
    pub image_preview: Option<String>,
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
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    pub requests: u64,
    #[serde(default)]
    pub cost_usd: f64,
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
    /// 按上游模型市场定价估算的累计消费。
    #[serde(default)]
    pub cost_usd: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenStats {
    pub today: TokenTotals,
    pub yesterday: TokenTotals,
    pub last7: TokenTotals,
    pub all_time: TokenTotals,
    pub series: Vec<DayTokens>,
    pub by_model: Vec<ModelTokens>,
    /// 每个模型最近 7 天的每日总 token，用于折线图多条线。
    #[serde(default)]
    pub model_trends: Vec<ModelDaySeries>,
}

/// 单个模型最近 7 天的每日总 token（与 series 同序、同长度）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDaySeries {
    pub model: String,
    pub daily: Vec<u64>,
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

/// 健康页一个格子的最小数据：用于前端按 status/stream_state/latency 着色。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthCell {
    pub status: u16,
    pub latency_ms: u64,
    pub stream_state: Option<String>,
}

/// 某个模型最近 N 条请求的健康格子（按时间倒序，最新在前）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelHealth {
    pub model: String,
    pub cells: Vec<HealthCell>,
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
        version: 15,
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
        // 不预设任何模型——预设模型挂在未登录的内置客户端 provider 上会造成
        // 「计数有数但页面空、删不掉、不可用还参与路由」的矛盾。用户自己按需添加。
        models: vec![],
        settings: Settings {
            bind_host: "127.0.0.1".into(),
            port: 8787,
            allow_lan: false,
            lan_api_key: default_lan_api_key(),
            lan_remote_host: String::new(),
            lan_remote_port: default_lan_remote_port(),
            lan_remote_api_key: String::new(),
            request_log_limit: 300,
            fallback_model: None,
            auto_inject: false,
            codex_default_model: None,
            codex_injection_mode: CodexInjectionMode::OfficialAccount,
            codex_internal_model_lock: true,
            codex_slots: Vec::new(),
            image_gen_model: None,
            aux_model: None,
            memory_model: None,
            direct_provider_id: None,
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

/// 仅供测试：在空预设之上注入旧的 3 个示例模型，让依赖模型的测试不受空预设影响。
#[cfg(test)]
pub(crate) fn seeded_config() -> AppConfig {
    let mut config = default_config();
    config.models = vec![
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
    ];
    config.settings.fallback_model = Some("gpt-5.5".into());
    config
}

#[cfg(test)]
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
        image_generation: false,
        image_quality: None,
        image_capable: false,
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
        // 画图模型无推理档位。
        ProviderProtocol::OpenAiImages | ProviderProtocol::GeminiImage => {
            (false, String::new(), Vec::new())
        }
    }
}

fn reasoning_levels(levels: &[&str]) -> Vec<String> {
    levels.iter().map(|level| (*level).to_string()).collect()
}

/// Codex 最新版的 `model_catalog_json` 不再支持 `max` 档。把模型存储的推理档位收敛到
/// Codex 的四档 `[low, medium, high, xhigh]`：Claude(anthropic)整体下移一档
/// (max→xhigh, xhigh→high, high→medium, medium→low, low 保持)，因为请求转发时
/// [`claude_request_reasoning_effort`] 会反向上移回 Claude 的真实档(含 max)；其余协议
/// 仅把越界的 `max` 收敛到 `xhigh`。两者互逆，保证档位往返一致。
pub fn codex_catalog_reasoning_level(level: &str, anthropic: bool) -> &'static str {
    match level.trim().to_ascii_lowercase().as_str() {
        "max" => "xhigh",
        "xhigh" if anthropic => "high",
        "high" if anthropic => "medium",
        "medium" if anthropic => "low",
        "low" => "low",
        "medium" => "medium",
        "high" => "high",
        "xhigh" => "xhigh",
        _ => "medium",
    }
}

/// 与 [`codex_catalog_reasoning_level`] 互逆：把 Codex 发来的四档上移回 Claude 的真实档。
/// 仅用于 anthropic 协议的请求转发。`max` 已是顶档，原样保留。
pub fn claude_request_reasoning_effort(level: &str) -> &'static str {
    match level.trim().to_ascii_lowercase().as_str() {
        "low" => "medium",
        "medium" => "high",
        "high" => "xhigh",
        "xhigh" => "max",
        _ => "max",
    }
}

#[cfg(test)]
fn default_timeout_ms() -> u64 {
    0
}

#[cfg(test)]
fn default_retry_count() -> u8 {
    0
}
