export type ProviderKind =
  | "official_open_ai"
  | "official_open_ai_account"
  | "official_anthropic_cli"
  | "official_anthropic_desktop"
  | "official_anthropic_account"
  | "custom";

export type ProviderProtocol =
  | "open_ai_responses"
  | "open_ai_chat_completions"
  | "anthropic_messages"
  | "open_ai_images";

export type ReasoningEffort = "low" | "medium" | "high" | "xhigh" | "max";
export type CodexInjectionMode = "official_account" | "third_party_api" | "lan_share";
export type StreamState =
  | "pending"
  | "completed"
  | "failed"
  | "cancelled"
  | "interrupted"
  | "incomplete"
  | "client_disconnected";

export type Provider = {
  id: string;
  name: string;
  kind: ProviderKind;
  protocol: ProviderProtocol;
  base_url: string;
  key_ref: string | null;
  http_proxy: ProviderHttpProxy;
};

export type ProviderHttpProxy = {
  enabled: boolean;
  url: string;
  username: string;
  password_ref: string | null;
};

export type ModelEntry = {
  id: string;
  display_name: string;
  description: string;
  context_window: number;
  enabled: boolean;
  provider_id: string;
  upstream_model: string | null;
  timeout_ms: number;
  retry_count: number;
  reasoning_enabled: boolean;
  default_reasoning_level: ReasoningEffort;
  supported_reasoning_levels: ReasoningEffort[];
  codex_alias: string | null;
  image_generation: boolean;
  image_quality: string | null;
};

export type CodexSlotAssignment = {
  mode: CodexInjectionMode;
  source: string;
  slot: string;
  target_model_id: string;
};

export type SettingsState = {
  bind_host: string;
  port: number;
  allow_lan: boolean;
  lan_api_key: string;
  lan_remote_host: string;
  lan_remote_port: number;
  lan_remote_api_key: string;
  request_log_limit: number;
  fallback_model: string | null;
  auto_inject: boolean;
  codex_default_model: string | null;
  codex_injection_mode: CodexInjectionMode;
  codex_internal_model_lock: boolean;
  codex_slots: CodexSlotAssignment[];
  image_gen_model: string | null;
};

export type AppConfig = {
  version: number;
  providers: Provider[];
  models: ModelEntry[];
  settings: SettingsState;
};

export type KeyStatus = {
  provider_id: string;
  present: boolean;
  available: boolean;
  message: string | null;
};

export type TokenUsage = {
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  total_tokens: number;
};

export type ContextBridgeDiagnostics = {
  original_body_bytes: number;
  final_body_bytes: number;
  original_tool_result_bytes: number;
  tool_result_count: number;
  context_management: boolean;
  last_message_role: string | null;
  last_message_content_type: string | null;
  last_message_text_length: number;
  last_message_preview_head: string | null;
  last_message_preview_tail: string | null;
  last_message_from_function_call_output: boolean;
  single_dot_user_message: boolean;
  latest_tool_result_count: number;
  latest_tool_result_text_length: number;
  latest_tool_result_single_dot: boolean;
  tool_results_truncated: number;
  tool_results_truncated_bytes: number;
  context_management_edits: string | null;
  applied_edits: string | null;
  compaction_persisted: boolean;
  compaction_injected: boolean;
};

export type RequestRecord = {
  id: string;
  started_at: string;
  model: string;
  requested_model: string | null;
  route_reason: string | null;
  provider_id: string | null;
  provider_name: string | null;
  provider_protocol: ProviderProtocol | null;
  status: number;
  latency_ms: number;
  streaming: boolean;
  error: string | null;
  reasoning_effort: ReasoningEffort | null;
  stream_state: StreamState | null;
  stream_error: string | null;
  last_event: string | null;
  stream_bytes: number;
  context_bridge: ContextBridgeDiagnostics | null;
  usage: TokenUsage;
  context_usage: TokenUsage;
  cost_usd: number | null;
  image_preview: string | null;
};

export type CodexConfigContent = {
  codex_home: string;
  config_path: string;
  content: string;
  exists: boolean;
};

export type CodexConfigSaveResult = {
  codex_home: string;
  config_path: string;
  backup_path: string;
};

export type RequestLogPage = {
  records: RequestRecord[];
  total: number;
  page: number;
  page_size: number;
};

export type TokenTotals = {
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  total_tokens: number;
  requests: number;
};

export type DayTokens = {
  date: string;
  total_tokens: number;
  input_tokens: number;
  output_tokens: number;
  requests: number;
};

export type ModelTokens = {
  model: string;
  total_tokens: number;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  requests: number;
};

export type TokenStats = {
  today: TokenTotals;
  yesterday: TokenTotals;
  last7: TokenTotals;
  all_time: TokenTotals;
  series: DayTokens[];
  by_model: ModelTokens[];
};

export type TestModelResult = {
  ok: boolean;
  status: number;
  latency_ms: number;
  reply: string;
  error: string | null;
  usage: TokenUsage;
  provider_name: string;
  image_preview: string | null;
};

export type ModelTestMode = "connectivity" | "image" | "context_400k" | "context_1m";

export type StartModelTestResult = {
  test_id: string;
};

export type ModelTestStatus = {
  test_id: string;
  mode: ModelTestMode;
  state: "running" | "completed" | "failed" | "cancelled";
  model: string;
  provider_name: string;
  stage: "queued" | "connectivity" | "probe" | "done" | "cancelled" | string;
  target_tokens: number;
  pass_threshold_tokens: number;
  current_tokens: number;
  current_estimated: boolean;
  confirmed_tokens: number;
  confirmed_estimated: boolean;
  last_status: number;
  latency_ms: number;
  last_error: string | null;
  summary: string | null;
  supported: boolean | null;
  inconclusive: boolean;
  result: TestModelResult | null;
};

export type ProviderCredential = {
  value: string;
  source: string;
  editable: boolean;
  deletable: boolean;
};

export type OAuthStart = {
  session_id: string;
  auth_url: string;
  expires_at: string;
};

export type OpenAiOAuthStart = OAuthStart;

export type CodexAppStatus = {
  running: boolean;
};

export type CodexAppRestartResult = {
  action: "started" | "restarted";
};

export type UpstreamModel = {
  id: string;
  label: string;
};

export type UpstreamModelList = {
  models: UpstreamModel[];
  error: string | null;
};

export type LanModelInfo = {
  id: string;
  display_name: string;
  description: string;
  context_window: number;
};

export type LanModelList = {
  models: LanModelInfo[];
};

export type OfficialQuotaWindow = {
  used_percent: number;
  limit_window_seconds: number;
  reset_after_seconds: number;
  reset_at: string | null;
};

export type OfficialAccountQuota = {
  account_id: string | null;
  user_id: string | null;
  email: string | null;
  plan_type: string | null;
  plan_label: string | null;
  subscription_expires_at: string | null;
  five_hour: OfficialQuotaWindow | null;
  seven_day: OfficialQuotaWindow | null;
  reset_credits: number | null;
};

export type ProviderLocalUsage = {
  provider_id: string;
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  total_tokens: number;
  requests: number;
  estimated_cost_usd: number | null;
  unknown_cost_models: string[];
};

export type ProviderUsageStatus = {
  provider_id: string;
  quota: OfficialAccountQuota | null;
  local_usage: ProviderLocalUsage;
  updated_at: string | null;
  source: string;
  error: string | null;
};

export type ImportResult = {
  scanned: number;
  imported: number;
  already: number;
  skipped: number;
  by_previous: Record<string, number>;
  sqlite_scanned: number;
  sqlite_updated: number;
  sqlite_already: number;
  sqlite_mismatched: number;
  backup_path: string | null;
  codex_home: string;
};

export type AppSnapshot = {
  config: AppConfig;
  keys: KeyStatus[];
  server: { bind_url: string; running: boolean; error: string | null };
  codex_apply_error: string | null;
  requests: RequestRecord[];
  request_log_count: number;
  stats: TokenStats;
  provider_usage: ProviderUsageStatus[];
  codex_home: string;
};

export type Page =
  | "dashboard"
  | "models"
  | "keys"
  | "logs"
  | "wizard"
  | "about";
