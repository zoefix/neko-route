import type {
  AppSnapshot,
  ModelTestMode,
  ModelTestStatus,
  StartModelTestResult,
  TestModelResult,
  TokenStats,
  UpstreamModel,
} from "./types";

export function mockUpstreamModels(providerId: string): UpstreamModel[] {
  if (providerId.includes("anthropic") || providerId.includes("claude")) {
    return [
      { id: "claude-opus-4-8", label: "Claude Opus 4.8" },
      { id: "claude-sonnet-4-5", label: "Claude Sonnet 4.5" },
      { id: "claude-haiku-4-5-20251001", label: "Claude Haiku 4.5" },
    ];
  }
  return [
    { id: "gpt-5.5", label: "gpt-5.5" },
    { id: "gpt-5.5-mini", label: "gpt-5.5-mini" },
    { id: "o4", label: "o4" },
    { id: "deepseek-v4-pro", label: "deepseek-v4-pro" },
  ];
}

function mockStats(): TokenStats {
  const today = new Date();
  const series = Array.from({ length: 7 }, (_, i) => {
    const d = new Date(today);
    d.setDate(today.getDate() - (6 - i));
    const total = [42000, 88000, 65000, 120000, 95000, 143000, 78000][i];
    return {
      date: d.toISOString().slice(0, 10),
      total_tokens: total,
      input_tokens: Math.round(total * 0.62),
      output_tokens: Math.round(total * 0.38),
      requests: Math.round(total / 3500),
    };
  });
  const mk = (t: number, r: number) => ({
    input_tokens: Math.round(t * 0.6),
    output_tokens: Math.round(t * 0.3),
    cache_read_tokens: Math.round(t * 0.08),
    cache_write_tokens: Math.round(t * 0.02),
    total_tokens: t,
    requests: r,
  });
  return {
    today: mk(78000, 22),
    yesterday: mk(143000, 41),
    last7: mk(631000, 188),
    all_time: mk(2940000, 845),
    series,
    by_model: [
      { model: "gpt-5.5", total_tokens: 1820000, input_tokens: 1100000, output_tokens: 560000, cache_read_tokens: 130000, cache_write_tokens: 30000, requests: 520 },
      { model: "claude-opus-4-8", total_tokens: 880000, input_tokens: 510000, output_tokens: 300000, cache_read_tokens: 56000, cache_write_tokens: 14000, requests: 240 },
      { model: "claude-sonnet-4-5", total_tokens: 240000, input_tokens: 150000, output_tokens: 78000, cache_read_tokens: 9000, cache_write_tokens: 3000, requests: 85 },
    ],
  };
}

export function mockTestModel(model: string): TestModelResult {
  return {
    ok: true,
    status: 200,
    latency_ms: 940,
    reply: "Hello! How can I help you today?",
    error: null,
    usage: {
      input_tokens: 8,
      output_tokens: 9,
      cache_read_tokens: 0,
      cache_write_tokens: 0,
      total_tokens: 17,
    },
    provider_name: model.startsWith("claude") ? "Claude Code CLI Official" : "OpenAI Official Account",
  };
}

const mockModelTests = new Map<string, ModelTestStatus>();

export function mockStartModelTest(model: string, mode: ModelTestMode): StartModelTestResult {
  const testId = `mock-${Date.now()}-${Math.random().toString(16).slice(2)}`;
  const target = mode === "context_1m" ? 1_000_000 : mode === "context_400k" ? 400_000 : 0;
  mockModelTests.set(testId, {
    test_id: testId,
    mode,
    state: "running",
    model,
    provider_name: model.startsWith("claude") ? "Claude Code CLI Official" : "OpenAI Official Account",
    stage: mode === "connectivity" ? "connectivity" : "probe",
    target_tokens: target,
    pass_threshold_tokens: Math.floor(target * 0.95),
    current_tokens: mode === "context_1m" ? 400_000 : mode === "context_400k" ? 128_000 : 0,
    current_estimated: mode !== "connectivity",
    confirmed_tokens: 0,
    confirmed_estimated: true,
    last_status: 0,
    latency_ms: 0,
    last_error: null,
    summary: null,
    supported: null,
    inconclusive: false,
    result: null,
  });
  return { test_id: testId };
}

export function mockGetModelTestStatus(testId: string): ModelTestStatus {
  const current = mockModelTests.get(testId);
  if (!current) throw new Error("Model test not found");
  if (current.state !== "running") return current;
  const nextCurrent =
    current.mode === "context_1m"
      ? Math.min(950_000, current.current_tokens + 275_000)
      : current.mode === "context_400k"
        ? Math.min(380_000, current.current_tokens + 126_000)
        : 0;
  const done = current.mode === "connectivity" || nextCurrent >= current.pass_threshold_tokens;
  const next: ModelTestStatus = {
    ...current,
    state: done ? "completed" : "running",
    stage: done ? "done" : "probe",
    current_tokens: nextCurrent,
    current_estimated: false,
    confirmed_tokens: nextCurrent,
    confirmed_estimated: false,
    last_status: 200,
    latency_ms: 900,
    supported: done ? true : null,
    summary: done ? "Model supports target context" : null,
    result: done ? mockTestModel(current.model) : null,
  };
  mockModelTests.set(testId, next);
  return next;
}

export function mockCancelModelTest(testId: string): ModelTestStatus {
  const current = mockModelTests.get(testId);
  if (!current) throw new Error("Model test not found");
  const next: ModelTestStatus = {
    ...current,
    state: "cancelled",
    stage: "cancelled",
    summary: "Test cancelled",
  };
  mockModelTests.set(testId, next);
  return next;
}

function mockHttpProxy() {
  return {
    enabled: false,
    url: "",
    username: "",
    password_ref: null,
  };
}

export function mockSnapshot(): AppSnapshot {
  return {
    config: {
      version: 11,
      providers: [
        {
          id: "openai-official",
          name: "OpenAI Official Account",
          kind: "official_open_ai",
          protocol: "open_ai_responses",
          base_url: "https://api.openai.com/v1",
          key_ref: null,
          http_proxy: mockHttpProxy(),
        },
        {
          id: "anthropic-cli",
          name: "Claude Code CLI Official",
          kind: "official_anthropic_cli",
          protocol: "anthropic_messages",
          base_url: "local://claude-code",
          key_ref: null,
          http_proxy: mockHttpProxy(),
        },
        {
          id: "anthropic-desktop",
          name: "Claude Desktop Official",
          kind: "official_anthropic_desktop",
          protocol: "anthropic_messages",
          base_url: "local://claude-desktop",
          key_ref: null,
          http_proxy: mockHttpProxy(),
        },
        {
          id: "custom-demo1234",
          name: "My Proxy",
          kind: "custom",
          protocol: "open_ai_chat_completions",
          base_url: "",
          key_ref: "provider:custom-demo1234",
          http_proxy: mockHttpProxy(),
        },
        {
          id: "openai-account-demo",
          name: "OpenAI Account",
          kind: "official_open_ai_account",
          protocol: "open_ai_responses",
          base_url: "https://api.openai.com/v1",
          key_ref: "official-token:openai-account-demo",
          http_proxy: mockHttpProxy(),
        },
        {
          id: "claude-account-demo",
          name: "Claude Account",
          kind: "official_anthropic_account",
          protocol: "anthropic_messages",
          base_url: "https://api.anthropic.com",
          key_ref: "official-token:claude-account-demo",
          http_proxy: mockHttpProxy(),
        },
      ],
      models: [
        {
          id: "gpt-5.5",
          display_name: "GPT-5.5",
          description: "OpenAI official account route",
          context_window: 1_000_000,
          enabled: true,
          provider_id: "openai-official",
          upstream_model: null,
          timeout_ms: 0,
          retry_count: 0,
          reasoning_enabled: true,
          default_reasoning_level: "xhigh",
          supported_reasoning_levels: ["low", "medium", "high", "xhigh"],
          codex_alias: null,
        },
        {
          id: "claude-opus-4-8",
          display_name: "Claude Opus 4.8",
          description: "Claude through Claude Code CLI credentials",
          context_window: 200_000,
          enabled: true,
          provider_id: "anthropic-cli",
          upstream_model: null,
          timeout_ms: 0,
          retry_count: 0,
          reasoning_enabled: true,
          default_reasoning_level: "max",
          supported_reasoning_levels: ["low", "medium", "high", "xhigh", "max"],
          codex_alias: null,
        },
        {
          id: "claude-sonnet-4-5",
          display_name: "Claude Sonnet 4.5",
          description: "Claude through Claude Desktop credentials",
          context_window: 200_000,
          enabled: false,
          provider_id: "anthropic-desktop",
          upstream_model: null,
          timeout_ms: 0,
          retry_count: 0,
          reasoning_enabled: true,
          default_reasoning_level: "max",
          supported_reasoning_levels: ["low", "medium", "high", "xhigh", "max"],
          codex_alias: null,
        },
      ],
      settings: {
        bind_host: "127.0.0.1",
        port: 8787,
        allow_lan: false,
        lan_api_key: "nr_demo_lan_key",
        lan_remote_host: "",
        lan_remote_port: 8787,
        lan_remote_api_key: "",
        request_log_limit: 300,
        fallback_model: "gpt-5.5",
        auto_inject: false,
        codex_default_model: null,
        codex_injection_mode: "official_account",
        codex_internal_model_lock: true,
        codex_slots: [],
      },
    },
    keys: [
      { provider_id: "openai-official", present: true, available: true, message: null },
      { provider_id: "anthropic-cli", present: true, available: true, message: null },
      {
        provider_id: "anthropic-desktop",
        present: false,
        available: false,
        message: "Claude Desktop 未登录",
      },
      { provider_id: "custom-demo1234", present: true, available: true, message: null },
      { provider_id: "openai-account-demo", present: true, available: true, message: null },
      { provider_id: "claude-account-demo", present: true, available: true, message: null },
    ],
    server: { bind_url: "http://127.0.0.1:8787/v1", running: true, error: null },
    codex_apply_error: null,
    requests: [
      {
        id: "1",
        started_at: new Date(Date.now() - 9000).toISOString(),
        model: "gpt-5.5",
        requested_model: null,
        route_reason: "direct",
        provider_id: "openai-official",
        provider_name: "OpenAI Official Account",
        provider_protocol: "open_ai_responses",
        status: 200,
        latency_ms: 842,
        streaming: true,
        error: null,
        reasoning_effort: "medium",
        stream_state: "completed",
        stream_error: null,
        last_event: "response.completed",
        stream_bytes: 0,
        context_bridge: null,
        usage: { input_tokens: 1240, output_tokens: 380, cache_read_tokens: 512, cache_write_tokens: 0, total_tokens: 1620 },
      },
      {
        id: "2",
        started_at: new Date(Date.now() - 30000).toISOString(),
        model: "claude-opus-4-8",
        requested_model: "gpt-5.4-mini",
        route_reason: "codex_internal_locked",
        provider_id: "anthropic-cli",
        provider_name: "Claude Code CLI Official",
        provider_protocol: "anthropic_messages",
        status: 200,
        latency_ms: 1320,
        streaming: true,
        error: null,
        reasoning_effort: "high",
        stream_state: "interrupted",
        stream_error: "network error: error decoding response body",
        last_event: "response.output_text.delta",
        stream_bytes: 42840,
        context_bridge: {
          strategy: "artifact_bridge_keep8",
          original_body_bytes: 1267480,
          final_body_bytes: 410320,
          original_tool_result_bytes: 1178417,
          tool_result_count: 42,
          kept_tool_results: 8,
          archived_tool_results: 34,
          archived_bytes: 820000,
          recalled_artifacts: 1,
          recalled_bytes: 1800,
          count_tokens_input_tokens: null,
          count_tokens_error: "count_tokens 404",
          context_management: true,
          raw_precheck_input_tokens: null,
          final_input_tokens: null,
          estimated_input_tokens: 969000,
          estimate_source: "previous_success_ratio",
          estimate_confidence: "medium",
          protection_triggered: true,
          target_input_tokens: 880000,
          previous_success_input_tokens: 969000,
          previous_success_body_bytes: 1267480,
          compression_stage: "preserve_12",
          protection_failure_reason: null,
          compression_reason: "body_size_fallback:1267480>704000",
          last_message_role: "user",
          last_message_content_type: "tool_result",
          last_message_text_length: 1,
          last_message_preview_head: ".",
          last_message_preview_tail: ".",
          last_message_from_function_call_output: true,
          single_dot_user_message: false,
          latest_tool_result_count: 1,
          latest_tool_result_text_length: 1,
          latest_tool_result_single_dot: true,
        },
        usage: { input_tokens: 860, output_tokens: 540, cache_read_tokens: 3200, cache_write_tokens: 1100, total_tokens: 5700 },
      },
      {
        id: "3",
        started_at: new Date(Date.now() - 64000).toISOString(),
        model: "claude-sonnet-4-5",
        requested_model: "gpt-5.3-codex",
        route_reason: "codex_slot",
        provider_id: "anthropic-desktop",
        provider_name: "Claude Desktop Official",
        provider_protocol: "anthropic_messages",
        status: 401,
        latency_ms: 120,
        streaming: false,
        error: "unauthorized",
        reasoning_effort: "max",
        stream_state: null,
        stream_error: null,
        last_event: null,
        stream_bytes: 0,
        context_bridge: null,
        usage: { input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0, total_tokens: 0 },
      },
    ],
    request_log_count: 3,
    stats: mockStats(),
    provider_usage: [
      {
        provider_id: "openai-account-demo",
        quota: {
          account_id: "acct_demo",
          user_id: "user_demo",
          email: "demo@example.com",
          plan_type: "pro_200x",
          plan_label: "Pro 200x",
          subscription_expires_at: "2026-12-31T00:00:00Z",
          five_hour: {
            used_percent: 42,
            limit_window_seconds: 18000,
            reset_after_seconds: 3600,
            reset_at: null,
          },
          seven_day: {
            used_percent: 18,
            limit_window_seconds: 604800,
            reset_after_seconds: 172800,
            reset_at: null,
          },
          reset_credits: 1,
        },
        local_usage: {
          provider_id: "openai-account-demo",
          input_tokens: 220000,
          output_tokens: 64000,
          cache_read_tokens: 12000,
          cache_write_tokens: 0,
          total_tokens: 296000,
          requests: 38,
          estimated_cost_usd: 0.92,
          unknown_cost_models: [],
        },
        updated_at: new Date().toISOString(),
        source: "live",
        error: null,
      },
      {
        provider_id: "claude-account-demo",
        quota: {
          account_id: "claude_account_demo",
          user_id: "claude_user_demo",
          email: "claude@example.com",
          plan_type: "max_20x",
          plan_label: "Max",
          subscription_expires_at: null,
          five_hour: {
            used_percent: 28,
            limit_window_seconds: 18000,
            reset_after_seconds: 2400,
            reset_at: null,
          },
          seven_day: {
            used_percent: 36,
            limit_window_seconds: 604800,
            reset_after_seconds: 259200,
            reset_at: null,
          },
          reset_credits: null,
        },
        local_usage: {
          provider_id: "claude-account-demo",
          input_tokens: 180000,
          output_tokens: 52000,
          cache_read_tokens: 9000,
          cache_write_tokens: 0,
          total_tokens: 241000,
          requests: 29,
          estimated_cost_usd: 1.24,
          unknown_cost_models: [],
        },
        updated_at: new Date().toISOString(),
        source: "live",
        error: null,
      },
    ],
    codex_home: "/Users/neko/.codex",
  };
}

export const isTauri = typeof (window as any).__TAURI_INTERNALS__ !== "undefined";
