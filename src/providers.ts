import type { Provider, ProviderProtocol, ReasoningEffort } from "./types";
import type { MsgKey } from "./messages";

export function isOfficialClaude(provider: Provider) {
  return (
    provider.kind === "official_anthropic_cli" ||
    provider.kind === "official_anthropic_desktop" ||
    provider.kind === "official_anthropic_account"
  );
}

export function isOfficialProvider(provider: Provider) {
  return provider.kind !== "custom";
}

export function claudeCredentialSource(provider: Provider): MsgKey | null {
  if (provider.kind === "official_anthropic_cli") return "kind.claudeCli";
  if (provider.kind === "official_anthropic_desktop") return "kind.claudeDesktop";
  return null;
}

export function providerKindKey(provider: Provider): MsgKey {
  switch (provider.kind) {
    case "official_open_ai":
      return "kind.openai";
    case "official_open_ai_account":
      return "kind.openaiAccount";
    case "official_anthropic_cli":
      return "kind.claudeCli";
    case "official_anthropic_desktop":
      return "kind.claudeDesktop";
    case "official_anthropic_account":
      return "kind.claudeAccount";
    case "custom":
      return "kind.custom";
  }
}

export function protocolKey(protocol: ProviderProtocol): MsgKey {
  switch (protocol) {
    case "open_ai_responses":
      return "proto.responses";
    case "open_ai_chat_completions":
      return "proto.chat";
    case "anthropic_messages":
      return "proto.anthropic";
  }
}

export function newCustomProvider(): Provider {
  const id = `custom-${crypto.randomUUID().slice(0, 8)}`;
  return {
    id,
    name: "Custom Provider",
    kind: "custom",
    protocol: "open_ai_responses",
    base_url: "https://api.example.com/v1",
    enabled: true,
    key_ref: `provider:${id}`,
  };
}

export function newOpenAiAccountProvider(): Provider {
  const id = `openai-account-${crypto.randomUUID().slice(0, 8)}`;
  return {
    id,
    name: "OpenAI Account",
    kind: "official_open_ai_account",
    protocol: "open_ai_responses",
    base_url: "https://api.openai.com/v1",
    enabled: true,
    key_ref: `official-token:${id}`,
  };
}

export function newClaudeAccountProvider(): Provider {
  const id = `claude-account-${crypto.randomUUID().slice(0, 8)}`;
  return {
    id,
    name: "Claude Account",
    kind: "official_anthropic_account",
    protocol: "anthropic_messages",
    base_url: "https://api.anthropic.com/v1",
    enabled: true,
    key_ref: `official-token:${id}`,
  };
}

export const OPENAI_REASONING_LEVELS: ReasoningEffort[] = ["low", "medium", "high", "xhigh"];
export const CLAUDE_REASONING_LEVELS: ReasoningEffort[] = ["low", "medium", "high", "xhigh", "max"];

export function reasoningDefaultsForProtocol(protocol: ProviderProtocol): {
  enabled: boolean;
  defaultLevel: ReasoningEffort;
  levels: ReasoningEffort[];
} {
  switch (protocol) {
    case "anthropic_messages":
      return { enabled: true, defaultLevel: "max", levels: [...CLAUDE_REASONING_LEVELS] };
    case "open_ai_responses":
      return { enabled: true, defaultLevel: "xhigh", levels: [...OPENAI_REASONING_LEVELS] };
    case "open_ai_chat_completions":
      return { enabled: true, defaultLevel: "xhigh", levels: [...OPENAI_REASONING_LEVELS] };
  }
}

/**
 * Smartly normalize a provider base URL for fault tolerance:
 *  - add `https://` when the scheme is missing,
 *  - drop a trailing slash,
 *  - append `/v1` only when the URL is "bare" (no version segment like
 *    /v1, /v1beta, /v4 and no known API path), so we don't break URLs that
 *    already carry a version or a deliberate custom route.
 *
 * Recognized shapes that are left untouched include OpenRouter
 * (`/api/v1`), Gemini OpenAI-compat (`/v1beta/openai`), Azure OpenAI
 * (`/openai/deployments/...`), and anything ending in an endpoint path.
 */
export function normalizeBaseUrl(raw: string): string {
  let input = raw.trim();
  if (!input) return input;
  if (!/^https?:\/\//i.test(input)) input = `https://${input}`;

  let url: URL;
  try {
    url = new URL(input);
  } catch {
    return input; // leave malformed input for backend validation to report
  }

  const segments = url.pathname.split("/").filter(Boolean);
  const lower = segments.map((s) => s.toLowerCase());

  const hasVersion = lower.some((s) => /^v\d+/.test(s) || s === "beta" || s === "alpha");
  const endpointWords = [
    "chat",
    "completions",
    "messages",
    "responses",
    "embeddings",
    "models",
    "deployments",
    "openai", // azure: /openai/deployments/...
  ];
  const hasEndpoint = lower.some((s) => endpointWords.includes(s));

  let path = segments.join("/");
  if (!hasVersion && !hasEndpoint) {
    if (segments.length === 0 || lower[lower.length - 1] === "api") {
      path = path ? `${path}/v1` : "v1";
    }
  }

  const rebuilt = `${url.protocol}//${url.host}${path ? `/${path}` : ""}`;
  return rebuilt;
}

export function formatContext(n: number) {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(n % 1_000_000 === 0 ? 0 : 1)}M`;
  if (n >= 1_000) return `${Math.round(n / 1000)}K`;
  return String(n);
}

export function formatTokens(n: number) {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(n >= 10_000_000 ? 0 : 2)}M`;
  if (n >= 10_000) return `${(n / 1000).toFixed(0)}K`;
  if (n >= 1_000) return `${(n / 1000).toFixed(1)}K`;
  return n.toLocaleString();
}
