import { invoke } from "@tauri-apps/api/core";
import type {
  AppConfig,
  AppSnapshot,
  CodexAppRestartResult,
  CodexAppStatus,
  CodexConfigContent,
  CodexConfigSaveResult,
  ImportResult,
  LanModelList,
  ModelTestMode,
  ModelHealth,
  ModelTestStatus,
  OAuthStart,
  ProviderCredential,
  RequestLogPage,
  ShareOverview,
  StartModelTestResult,
  TestModelResult,
  UpstreamModelList,
} from "./types";
import { isTauri, mockCancelModelTest, mockCreateShareToken, mockDeleteShareToken, mockGetModelTestStatus, mockModelHealth, mockSetShareEnabled, mockShareOverview, mockUpdateShareToken, mockSnapshot, mockStartModelTest, mockTestModel, mockUpstreamModels } from "./mock";

// In-memory snapshot used only when running in a plain browser (web:dev preview).
let demo: AppSnapshot | null = null;
function demoSnap(): AppSnapshot {
  if (!demo) demo = mockSnapshot();
  return demo;
}

export const api = {
  getSnapshot: () =>
    isTauri ? invoke<AppSnapshot>("get_snapshot") : Promise.resolve(demoSnap()),
  shareOverview: () =>
    isTauri ? invoke<ShareOverview>("share_overview") : Promise.resolve(mockShareOverview()),
  setShareEnabled: (enabled: boolean) =>
    isTauri
      ? invoke<ShareOverview>("set_share_enabled", { enabled })
      : Promise.resolve(mockSetShareEnabled(enabled)),
  createShareToken: (
    token: string,
    label: string,
    allowedModelIds: string[],
    amountLimitUsd: number | null,
    concurrencyLimit: number,
    rpmLimit: number,
    modelAliases: Record<string, string>,
  ) =>
    isTauri
      ? invoke<ShareOverview>("create_share_token", {
          token,
          label,
          allowedModelIds,
          amountLimitUsd,
          concurrencyLimit,
          rpmLimit,
          modelAliases,
        })
      : Promise.resolve(
          mockCreateShareToken(
            token,
            label,
            allowedModelIds,
            amountLimitUsd,
            concurrencyLimit,
            rpmLimit,
            modelAliases,
          ),
        ),
  updateShareToken: (
    token: string,
    newToken: string,
    label: string,
    allowedModelIds: string[],
    amountLimitUsd: number | null,
    concurrencyLimit: number,
    rpmLimit: number,
    modelAliases: Record<string, string>,
  ) =>
    isTauri
      ? invoke<ShareOverview>("update_share_token", {
          token,
          newToken,
          label,
          allowedModelIds,
          amountLimitUsd,
          concurrencyLimit,
          rpmLimit,
          modelAliases,
        })
      : Promise.resolve(
          mockUpdateShareToken(
            token,
            newToken,
            label,
            allowedModelIds,
            amountLimitUsd,
            concurrencyLimit,
            rpmLimit,
            modelAliases,
          ),
        ),
  deleteShareToken: (token: string) =>
    isTauri
      ? invoke<ShareOverview>("delete_share_token", { token })
      : Promise.resolve(mockDeleteShareToken(token)),

  saveConfig: (config: AppConfig) => {
    if (!isTauri) {
      const prev = demoSnap();
      // mirror backend: keep key statuses, drop removed providers, default new ones
      const keys = config.providers.map((p) => {
        const existing = prev.keys.find((k) => k.provider_id === p.id);
        return existing ?? { provider_id: p.id, present: false, available: true, message: null };
      });
      demo = { ...prev, config, keys };
      return Promise.resolve(demo);
    }
    return invoke<AppSnapshot>("save_config", { config });
  },

  regenerateLanApiKey: () => {
    if (!isTauri) {
      const prev = demoSnap();
      demo = {
        ...prev,
        config: {
          ...prev.config,
          settings: {
            ...prev.config.settings,
            lan_api_key: `nr_demo_${Math.random().toString(36).slice(2)}`,
          },
        },
      };
      return Promise.resolve(demo);
    }
    return invoke<AppSnapshot>("regenerate_lan_api_key");
  },

  listLanModels: () =>
    isTauri
      ? invoke<LanModelList>("list_lan_models")
      : Promise.resolve({
          models: [
            {
              id: "gpt-5.5",
              display_name: "GPT-5.5",
              description: "LAN shared model",
              context_window: 1_000_000,
            },
          ],
        } as LanModelList),

  setProviderKey: (providerId: string, secret: string) => {
    if (!isTauri) {
      const prev = demoSnap();
      demo = {
        ...prev,
        keys: prev.keys.map((k) =>
          k.provider_id === providerId ? { ...k, present: secret.length > 0, available: true, message: null } : k,
        ),
      };
      return Promise.resolve(demo);
    }
    return invoke<AppSnapshot>("set_provider_key", { providerId, secret });
  },

  setOfficialProviderToken: (providerId: string, tokenJson: string) => {
    if (!isTauri) {
      const prev = demoSnap();
      demo = {
        ...prev,
        keys: prev.keys.map((k) =>
          k.provider_id === providerId ? { ...k, present: tokenJson.trim().length > 0, available: true, message: null } : k,
        ),
      };
      return Promise.resolve(demo);
    }
    return invoke<AppSnapshot>("set_official_provider_token", { providerId, tokenJson });
  },

  deleteOfficialProviderToken: (providerId: string) => {
    if (!isTauri) {
      const prev = demoSnap();
      demo = {
        ...prev,
        keys: prev.keys.map((k) => (k.provider_id === providerId ? { ...k, present: false, available: false } : k)),
      };
      return Promise.resolve(demo);
    }
    return invoke<AppSnapshot>("delete_official_provider_token", { providerId });
  },

  refreshOfficialProviderToken: (providerId: string) =>
    isTauri ? invoke<AppSnapshot>("refresh_official_provider_token", { providerId }) : Promise.resolve(demoSnap()),

  startOpenAiOAuth: () =>
    isTauri
      ? invoke<OAuthStart>("start_openai_oauth")
      : Promise.resolve({
          session_id: "demo-session",
          auth_url: "https://auth.openai.com/oauth/authorize?demo=1",
          expires_at: new Date(Date.now() + 30 * 60 * 1000).toISOString(),
        } as OAuthStart),

  finishOpenAiOAuth: (providerId: string, sessionId: string, callbackOrCode: string) => {
    if (!isTauri) {
      const prev = demoSnap();
      demo = {
        ...prev,
        keys: prev.keys.map((k) =>
          k.provider_id === providerId ? { ...k, present: callbackOrCode.trim().length > 0, available: true, message: null } : k,
        ),
      };
      return Promise.resolve(demo);
    }
    return invoke<AppSnapshot>("finish_openai_oauth", { providerId, sessionId, callbackOrCode });
  },

  startClaudeOAuth: () =>
    isTauri
      ? invoke<OAuthStart>("start_claude_oauth")
      : Promise.resolve({
          session_id: "demo-claude-session",
          auth_url: "https://claude.ai/oauth/authorize?demo=1",
          expires_at: new Date(Date.now() + 30 * 60 * 1000).toISOString(),
        } as OAuthStart),

  finishClaudeOAuth: (providerId: string, sessionId: string, callbackOrCode: string) => {
    if (!isTauri) {
      const prev = demoSnap();
      demo = {
        ...prev,
        keys: prev.keys.map((k) =>
          k.provider_id === providerId ? { ...k, present: callbackOrCode.trim().length > 0, available: true, message: null } : k,
        ),
      };
      return Promise.resolve(demo);
    }
    return invoke<AppSnapshot>("finish_claude_oauth", { providerId, sessionId, callbackOrCode });
  },

  finishClaudeCookieOAuth: (providerId: string, sessionKey: string) => {
    if (!isTauri) {
      const prev = demoSnap();
      demo = {
        ...prev,
        keys: prev.keys.map((k) =>
          k.provider_id === providerId ? { ...k, present: sessionKey.trim().length > 0, available: true, message: null } : k,
        ),
      };
      return Promise.resolve(demo);
    }
    return invoke<AppSnapshot>("finish_claude_cookie_oauth", { providerId, sessionKey });
  },

  readProviderCredential: (providerId: string) => {
    if (!isTauri) {
      const provider = demoSnap().config.providers.find((p) => p.id === providerId);
      return Promise.resolve({
        value: provider?.kind === "custom" ? "sk-demo-secret" : '{\n  "access_token": "demo-token"\n}',
        source: provider?.kind === "custom" ? "Neko Route local storage" : "Demo credential source",
        editable: provider?.kind === "custom" || provider?.kind === "official_open_ai_account" || provider?.kind === "official_anthropic_account",
        deletable: provider?.kind === "custom" || provider?.kind === "official_open_ai_account" || provider?.kind === "official_anthropic_account",
      } as ProviderCredential);
    }
    return invoke<ProviderCredential>("read_provider_credential", { providerId });
  },

  deleteProviderKey: (providerId: string) => {
    if (!isTauri) {
      const prev = demoSnap();
      demo = {
        ...prev,
        keys: prev.keys.map((k) => (k.provider_id === providerId ? { ...k, present: false } : k)),
      };
      return Promise.resolve(demo);
    }
    return invoke<AppSnapshot>("delete_provider_key", { providerId });
  },

  readProviderProxyPassword: (providerId: string) => {
    if (!isTauri) {
      const provider = demoSnap().config.providers.find((p) => p.id === providerId);
      return Promise.resolve(provider?.http_proxy.password_ref ? "demo-proxy-password" : "");
    }
    return invoke<string>("read_provider_proxy_password", { providerId });
  },

  setProviderProxyPassword: (providerId: string, password: string) => {
    if (!isTauri) return Promise.resolve(demoSnap());
    return invoke<AppSnapshot>("set_provider_proxy_password", { providerId, password });
  },

  deleteProviderProxyPassword: (providerId: string) => {
    if (!isTauri) return Promise.resolve(demoSnap());
    return invoke<AppSnapshot>("delete_provider_proxy_password", { providerId });
  },

  testRoute: (model: string) =>
    isTauri
      ? invoke<Record<string, unknown>>("test_route", { model })
      : Promise.resolve({
          model,
          note: "browser demo — connect via Tauri for live routing",
        } as Record<string, unknown>),

  testModel: (model: string, providerId?: string) =>
    isTauri
      ? invoke<TestModelResult>("test_model", { model, providerId })
      : Promise.resolve(mockTestModel(model)),

  startModelTest: (model: string, providerId: string | undefined, mode: ModelTestMode) =>
    isTauri
      ? invoke<StartModelTestResult>("start_model_test", { model, providerId, mode })
      : Promise.resolve(mockStartModelTest(model, mode)),

  getModelTestStatus: (testId: string) =>
    isTauri
      ? invoke<ModelTestStatus>("get_model_test_status", { testId })
      : Promise.resolve(mockGetModelTestStatus(testId)),

  cancelModelTest: (testId: string) =>
    isTauri
      ? invoke<ModelTestStatus>("cancel_model_test", { testId })
      : Promise.resolve(mockCancelModelTest(testId)),

  listUpstreamModels: (providerId: string) =>
    isTauri
      ? invoke<UpstreamModelList>("list_upstream_models", { providerId })
      : Promise.resolve({ models: mockUpstreamModels(providerId), error: null }),

  modelHealth: (models: string[]) =>
    isTauri ? invoke<ModelHealth[]>("model_health", { models }) : Promise.resolve(mockModelHealth(models)),

  refreshProviderUsage: (providerId: string) =>
    isTauri
      ? invoke<AppSnapshot>("refresh_provider_usage", { providerId })
      : Promise.resolve(demoSnap()),

  codexAppStatus: () =>
    isTauri
      ? invoke<CodexAppStatus>("codex_app_status")
      : Promise.resolve({ running: false } as CodexAppStatus),

  restartCodexApp: () =>
    isTauri
      ? invoke<CodexAppRestartResult>("restart_codex_app")
      : Promise.resolve({ action: "started" } as CodexAppRestartResult),

  exportCatalog: () =>
    isTauri ? invoke<string>("export_catalog") : Promise.resolve("/demo/.codex/model-catalogs/neko-route.json"),

  installCodexConfig: (defaultModel: string) =>
    isTauri
      ? invoke<Record<string, string>>("install_codex_config", { defaultModel })
      : Promise.resolve({ config_path: "/demo/.codex/config.toml" }),

  restoreCodexConfig: (deleteCatalog: boolean) =>
    isTauri
      ? invoke<Record<string, string>>("restore_codex_config", { deleteCatalog })
      : Promise.resolve({ config_path: "/demo/.codex/config.toml" }),

  readCodexConfig: () =>
    isTauri
      ? invoke<CodexConfigContent>("read_codex_config")
      : Promise.resolve({
          codex_home: "/demo/.codex",
          config_path: "/demo/.codex/config.toml",
          content:
            'model_provider = "neko-route"\nmodel_catalog_json = "/demo/.codex/model-catalogs/neko-route.json"\n\n[model_providers.neko-route]\nname = "neko-route"\nbase_url = "http://127.0.0.1:8787/v1"\nwire_api = "responses"\nrequires_openai_auth = true\n',
          exists: true,
        } as CodexConfigContent),

  saveCodexConfig: (content: string) =>
    isTauri
      ? invoke<CodexConfigSaveResult>("save_codex_config", { content })
      : Promise.resolve({
          codex_home: "/demo/.codex",
          config_path: "/demo/.codex/config.toml",
          backup_path: "/demo/.codex/config-backups/neko-route-manual-demo.toml",
        } as CodexConfigSaveResult),

  importSessions: () =>
    isTauri
      ? invoke<ImportResult>("import_sessions")
      : Promise.resolve({
          scanned: 128,
          imported: 115,
          already: 13,
          skipped: 0,
          by_previous: { custom: 102, capture: 13 },
          sqlite_scanned: 128,
          sqlite_updated: 115,
          sqlite_already: 13,
          sqlite_mismatched: 115,
          backup_path: "/demo/.codex/config-backups/neko-route-session-import-demo",
          codex_home: "/demo/.codex",
        } as ImportResult),

  getRequestLogs: (
    page: number,
    pageSize: number,
    shareToken?: string | null,
  ) => {
    if (!isTauri) {
      let requests = demoSnap().requests;
      if (shareToken === "__local__") {
        requests = requests.filter((r) => !r.context_bridge?.share_token);
      } else if (shareToken) {
        requests = requests.filter(
          (r) => r.context_bridge?.share_token === shareToken,
        );
      }
      const offset = (Math.max(1, page) - 1) * pageSize;
      return Promise.resolve({
        records: requests.slice(offset, offset + pageSize),
        total: requests.length,
        page,
        page_size: pageSize,
      } as RequestLogPage);
    }
    return invoke<RequestLogPage>("get_request_logs", {
      page,
      pageSize,
      shareToken: shareToken ?? null,
    });
  },

  clearRequestLogs: () => {
    if (!isTauri) {
      demo = { ...demoSnap(), requests: [], request_log_count: 0 };
      return Promise.resolve(undefined);
    }
    return invoke("clear_request_logs");
  },

  readImagePreview: (name: string): Promise<string> => {
    if (!isTauri) return Promise.resolve("");
    return invoke<string>("read_image_preview", { name });
  },
};
