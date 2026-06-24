import React from "react";
import { createPortal } from "react-dom";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  IconActivity as Activity,
  IconBook2 as BookOpen,
  IconBrandBilibili as BrandBilibili,
  IconBrandGithub as BrandGithub,
  IconBrandTiktok as BrandTiktok,
  IconBrandX as BrandX,
  IconBrandYoutube as BrandYoutube,
  IconCloudDownload as DownloadCloud,
  IconCoins as Coins,
  IconCopy as Copy,
  IconCpu as Cpu,
  IconEye as Eye,
  IconEyeOff as EyeOff,
  IconFileCode as FileJson,
  IconGauge as Gauge,
  IconGripVertical as GripVertical,
  IconInbox as Inbox,
  IconPhoto as ImageIcon,
  IconExternalLink as ExternalLink,
  IconListTree as ListTree,
  IconPencil as Pencil,
  IconPlugConnected as CustomProviderIcon,
  IconPlayerPlay as Play,
  IconPlus as Plus,
  IconRefresh as RotateCcw,
  IconRocket as Rocket,
  IconRouteAltLeft as RouteProxy,
  IconServer as Server,
  IconSettings as Settings2,
  IconShieldCheck as ShieldCheck,
  IconSparkles as Sparkles,
  IconTrash as Trash2,
} from "@tabler/icons-react";
import { api } from "./api";
import { useI18n } from "./i18n";
import type { MsgKey } from "./messages";
import type {
  AppConfig,
  AppSnapshot,
  CodexInjectionMode,
  LanModelInfo,
  ModelTestMode,
  ModelTestStatus,
  ModelEntry,
  OfficialQuotaWindow,
  Provider,
  ProviderHttpProxy,
  ProviderUsageStatus,
  ProviderProtocol,
  ReasoningEffort,
  TokenTotals,
} from "./types";
import {
  formatContext,
  formatTokens,
  formatCost,
  isOfficialClaude,
  newClaudeAccountProvider,
  newCustomProvider,
  newOpenAiAccountProvider,
  normalizeBaseUrl,
  protocolKey,
  reasoningDefaultsForProtocol,
} from "./providers";
import {
  formatBytes,
  isTauriRuntime,
  type ReleaseNotes,
  type UpdateStatus,
} from "./updates";
import { TrendChart } from "./TrendChart";
import {
  Button,
  Combobox,
  ConfirmDialog,
  Dropdown,
  Empty,
  Field,
  IconButton,
  Input,
  Modal,
  Panel,
  Pill,
  Stat,
  Switch,
  useSeedOnOpen,
} from "./ui";
import type { Option } from "./ui";
import providerClaudeIcon from "./assets/provider-claude.png";
import providerOpenAiIcon from "./assets/provider-openai.png";
import appIcon from "./assets/app-icon.png";

export type PageProps = {
  snapshot: AppSnapshot;
  config: AppConfig;
  commit: (updater: (draft: AppConfig) => void, toastKey?: MsgKey) => Promise<boolean>;
  refresh: () => Promise<void>;
  notify: (key: MsgKey, tone?: "ok" | "bad") => void;
  notifyRaw: (msg: string, tone?: "ok" | "bad") => void;
  busy: boolean;
  setBusy: (v: boolean) => void;
  appVersion: string;
  updateStatus: UpdateStatus;
  availableUpdateVersion: string | null;
  currentRelease: ReleaseNotes | null;
  currentReleaseLoading: boolean;
  currentReleaseError: string;
  checkForUpdate: () => Promise<void>;
};

function providerIcon(p: Provider) {
  if (p.kind === "official_open_ai" || p.kind === "official_open_ai_account") {
    return { icon: <img src={providerOpenAiIcon} alt="" className="brand-img" />, cls: "openai" };
  }
  if (isOfficialClaude(p)) return { icon: <img src={providerClaudeIcon} alt="" className="brand-img" />, cls: "claude" };
  return { icon: <CustomProviderIcon size={20} />, cls: "custom" };
}

function isOfficialAccountProvider(provider: Provider) {
  return (
    provider.kind === "official_open_ai_account" ||
    provider.kind === "official_anthropic_account"
  );
}

function isBuiltInOfficialClient(provider: Provider) {
  return (
    provider.kind === "official_open_ai" ||
    provider.kind === "official_anthropic_cli" ||
    provider.kind === "official_anthropic_desktop"
  );
}

function providerVisibleInUi(provider: Provider, snapshot: AppSnapshot) {
  if (!isBuiltInOfficialClient(provider)) return true;
  const status = snapshot.keys.find((item) => item.provider_id === provider.id);
  return Boolean(status?.present && status.available !== false);
}

export function visibleUiProviders(config: AppConfig, snapshot: AppSnapshot) {
  return config.providers.filter((provider) => providerVisibleInUi(provider, snapshot));
}

function visibleUiProviderIds(config: AppConfig, snapshot: AppSnapshot) {
  return new Set(visibleUiProviders(config, snapshot).map((provider) => provider.id));
}

/// 左侧导航/计数用：可见 provider(已登录或第三方)下的模型数；未登录客户端的模型不计入。
export function visibleUiModelCount(config: AppConfig, snapshot: AppSnapshot) {
  const ids = visibleUiProviderIds(config, snapshot);
  return config.models.filter((model) => ids.has(model.provider_id)).length;
}

function providerShortSourceKey(provider?: Provider): MsgKey {
  if (!provider) return "source.thirdParty";
  switch (provider.kind) {
    case "official_open_ai":
    case "official_open_ai_account":
      return "source.openai";
    case "official_anthropic_cli":
      return "source.claudeCli";
    case "official_anthropic_desktop":
      return "source.claudeDesktop";
    case "official_anthropic_account":
      return "source.claudeAccount";
    case "custom":
      return "source.thirdParty";
  }
}

function formatUsd(value: number | null | undefined) {
  if (value == null || !Number.isFinite(value)) return "--";
  if (value > 0 && value < 0.01) return `$${value.toFixed(4)}`;
  return `$${value.toFixed(2)}`;
}

function formatQuotaPercent(value: number | null | undefined) {
  if (value == null || !Number.isFinite(value)) return "--";
  return `${Math.max(0, Math.min(100, value)).toFixed(value >= 10 ? 0 : 1)}%`;
}

function quotaResetText(window?: OfficialQuotaWindow | null) {
  if (!window) return "";
  const resetAtMs = window.reset_at ? Date.parse(window.reset_at) : NaN;
  const seconds = Number.isFinite(resetAtMs)
    ? Math.max(0, Math.ceil((resetAtMs - Date.now()) / 1000))
    : window.reset_after_seconds;
  if (!seconds) return "";

  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.ceil((seconds % 3600) / 60);

  if (days > 0) {
    return hours > 0 ? `${days}d ${hours}h` : `${days}d`;
  }
  if (hours > 0) {
    return minutes > 0 ? `${hours}h ${minutes}m` : `${hours}h`;
  }
  return `${Math.max(1, minutes)}m`;
}

function formatLatency(ms: number) {
  const seconds = Math.round((Math.max(0, ms) / 1000) * 10) / 10;
  const value = Number.isInteger(seconds) ? seconds.toFixed(0) : seconds.toFixed(1);
  return `${value} s`;
}

function latencyTone(ms: number) {
  const seconds = Math.max(0, ms) / 1000;
  if (seconds < 10) return "good";
  if (seconds < 60) return "warn";
  return "bad";
}

type RequestRecordView = AppSnapshot["requests"][number];

function streamStateLabel(t: (key: MsgKey, vars?: Record<string, string | number>) => string, state: RequestRecordView["stream_state"]) {
  switch (state) {
    case "pending":
      return t("stream.pending");
    case "completed":
      return t("stream.completed");
    case "failed":
      return t("stream.failed");
    case "interrupted":
      return t("stream.interrupted");
    case "incomplete":
      return t("stream.incomplete");
    case "client_disconnected":
      return t("stream.clientDisconnected");
    default:
      return "";
  }
}

function streamStatusDisplay(
  record: RequestRecordView,
  t: (key: MsgKey, vars?: Record<string, string | number>) => string,
) {
  const streamLabel = streamStateLabel(t, record.stream_state);
  const streamSize = formatBytes(record.stream_bytes);
  const latency = formatLatency(record.latency_ms);
  const title = [
    `${record.latency_ms}ms`,
    streamLabel || record.stream_state,
    `${t("tokens.streamBytes")}: ${streamSize}`,
    record.last_event,
    record.stream_error,
  ].filter(Boolean).join(" · ");

  if (record.stream_state === "pending") {
    return { label: streamSize, tone: "good", title };
  }
  if (record.stream_state === "failed") {
    return { label: streamLabel, tone: "bad", title };
  }
  // 客户端断开但已 2xx 完成(token 下发完毕)——这类其实是正常完成的，显示最终耗时(绿)而非"已断开"。
  if (record.stream_state === "client_disconnected" && record.status < 400) {
    return { label: latency, tone: "good", title };
  }
  if (
    record.stream_state === "interrupted" ||
    record.stream_state === "incomplete" ||
    record.stream_state === "client_disconnected"
  ) {
    return { label: streamLabel, tone: "bad", title };
  }
  return { label: latency, tone: latencyTone(record.latency_ms), title };
}

function requestDisplayModel(record: RequestRecordView, models: ModelEntry[]) {
  // 模型 ID 现在是随机的 neko-model-xxx，日志显示用户填的显示名称更直观。
  const found = models.find((model) => model.id === record.model);
  return found?.display_name || record.model || record.requested_model || "—";
}

function requestErrorDetail(
  record: RequestRecordView,
  t: (key: MsgKey, vars?: Record<string, string | number>) => string,
  models: ModelEntry[],
) {
  const lines = [
    `${t("table.status")}: ${record.status}`,
    `${t("table.model")}: ${requestDisplayModel(record, models)}`,
  ];
  if (record.requested_model && record.requested_model !== record.model) {
    lines.push(t("table.requestedModel", { model: record.requested_model }));
  }
  if (record.provider_name) {
    lines.push(`${t("table.provider")}: ${record.provider_name}`);
  }
  if (record.provider_protocol) {
    lines.push(`${t("table.protocol")}: ${t(protocolKey(record.provider_protocol))}`);
  }
  if (record.stream_state) {
    lines.push(`${t("table.stream")}: ${streamStatusDisplay(record, t).label}`);
  }
  if (record.last_event) {
    lines.push(`Last event: ${record.last_event}`);
  }
  if (record.stream_bytes > 0) {
    lines.push(`${t("tokens.streamBytes")}: ${formatBytes(record.stream_bytes)}`);
  }
  if (record.context_bridge) {
    const bridge = record.context_bridge;
    lines.push(
      "",
      `${t("logs.contextBridgeOriginal")}: ${formatBytes(bridge.original_body_bytes)}`,
      `${t("logs.contextBridgeFinal")}: ${formatBytes(bridge.final_body_bytes)}`,
      `${t("logs.contextBridgeToolResults")}: ${bridge.tool_result_count} · ${formatBytes(bridge.original_tool_result_bytes)}`,
      `${t("logs.contextBridgeTruncated")}: ${bridge.tool_results_truncated} · ${formatBytes(bridge.tool_results_truncated_bytes)}`,
      `${t("logs.contextBridgeLastMessage")}: ${bridge.last_message_role || "—"} · ${
        bridge.last_message_content_type || "—"
      } · ${formatTokens(bridge.last_message_text_length)}`,
      `${t("logs.contextBridgeLastPreview")}: ${
        bridge.last_message_preview_head || "—"
      }${bridge.last_message_preview_tail && bridge.last_message_preview_tail !== bridge.last_message_preview_head ? ` ... ${bridge.last_message_preview_tail}` : ""}`,
      `${t("logs.contextBridgeSingleDot")}: user=${
        bridge.single_dot_user_message ? "yes" : "no"
      } · tool=${bridge.latest_tool_result_single_dot ? "yes" : "no"} · function_call_output=${
        bridge.last_message_from_function_call_output ? "yes" : "no"
      }`,
      `${t("logs.contextBridgeLatestTool")}: ${bridge.latest_tool_result_count} · ${formatTokens(
        bridge.latest_tool_result_text_length,
      )}`,
      `${t("logs.contextBridgeManagement")}: ${bridge.context_management ? "on" : "off"}${
        bridge.context_management_edits ? ` · ${bridge.context_management_edits}` : ""
      }${bridge.applied_edits ? ` · applied=${bridge.applied_edits}` : ""}`,
      `${t("logs.contextBridgeCompaction")}: persisted=${
        bridge.compaction_persisted ? "yes" : "no"
      } · injected=${bridge.compaction_injected ? "yes" : "no"}`,
    );
  }
  if (record.error) {
    lines.push("", record.error);
  }
  if (record.stream_error && record.stream_error !== record.error) {
    lines.push("", record.stream_error);
  }
  return lines.join("\n");
}

/* ============================================================
   About
   ============================================================ */
export function About({
  notify,
  notifyRaw,
  appVersion,
  updateStatus,
  availableUpdateVersion,
  currentRelease,
  currentReleaseLoading,
  currentReleaseError,
  checkForUpdate,
}: PageProps) {
  const { t } = useI18n();

  async function openExternal(url: string) {
    try {
      if (isTauriRuntime()) {
        await openUrl(url);
      } else {
        window.open(url, "_blank", "noopener,noreferrer");
      }
    } catch (error) {
      notifyRaw(String(error), "bad");
    }
  }

  async function copyText(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      notify("toast.copied");
    } catch (error) {
      notifyRaw(String(error), "bad");
    }
  }

  const socialLinks = [
    {
      label: t("about.github"),
      value: "zoefix/neko-route",
      icon: <BrandGithub size={22} />,
      url: "https://github.com/zoefix/neko-route",
    },
    {
      label: t("about.douyin"),
      value: "zoefix",
      icon: <BrandTiktok size={22} />,
      copy: "zoefix",
    },
    {
      label: t("about.bilibili"),
      value: "space.bilibili.com/17415536",
      icon: <BrandBilibili size={22} />,
      url: "https://space.bilibili.com/17415536",
    },
    {
      label: t("about.xiaohongshu"),
      value: "zoefix",
      icon: <BookOpen size={22} />,
      copy: "zoefix",
    },
    {
      label: t("about.twitter"),
      value: "zoefech",
      icon: <BrandX size={22} />,
      url: "https://x.com/zoefech",
    },
    {
      label: "YouTube",
      value: "@zoefyx",
      icon: <BrandYoutube size={22} />,
      url: "https://www.youtube.com/@zoefyx",
    },
  ];
  const showUpdateBadge =
    Boolean(availableUpdateVersion) &&
    (updateStatus === "available" ||
      updateStatus === "downloading" ||
      updateStatus === "installing" ||
      updateStatus === "restarting");
  const versionBadgeLabel = showUpdateBadge
    ? t("about.versionUpdateAvailable", { version: availableUpdateVersion ?? "" })
    : t("about.versionLatest");

  return (
    <div className="about-page page-enter">
      <section className="about-hero">
        <div className="about-app">
          <span className="about-app-icon"><img src={appIcon} alt="" /></span>
          <div>
            <h2>Neko Route</h2>
            <div className="about-version">
              <span>{t("about.currentVersion")}</span>
              <strong>{appVersion}</strong>
              <span className={`about-version-badge ${showUpdateBadge ? "available" : "latest"}`}>
                <span />
                {versionBadgeLabel}
              </span>
            </div>
          </div>
        </div>
        <Button
          variant="ghost"
          icon={<RotateCcw size={18} />}
          onClick={checkForUpdate}
          loading={updateStatus === "checking"}
          disabled={updateStatus === "downloading" || updateStatus === "installing" || updateStatus === "restarting"}
          className="about-check-btn"
        >
          {t("about.checkUpdate")}
        </Button>
      </section>

      <section className="about-grid">
        <div className="about-card">
          <div className="about-card-head">
            <span><ExternalLink size={18} /></span>
            <div>
              <h3>{t("about.officialInfo")}</h3>
              <p>{t("about.officialInfoSub")}</p>
            </div>
          </div>
          <div className="about-links">
            {socialLinks.map((item) => (
              <div className="about-link-row" key={item.label}>
                <span className="about-link-icon">{item.icon}</span>
                <strong>{item.label}</strong>
                <span>{item.value}</span>
                {item.url ? (
                  <IconButton icon={<ExternalLink size={16} />} title={t("about.open")} onClick={() => openExternal(item.url)} />
                ) : (
                  <IconButton icon={<Copy size={16} />} title={t("about.copy")} onClick={() => copyText(item.copy ?? item.value)} />
                )}
              </div>
            ))}
          </div>
        </div>

        <div className="about-card about-update-card">
          <div className="about-card-head">
            <span><Rocket size={18} /></span>
            <div>
              <h3>{t("about.currentReleaseTitle")}</h3>
              <p>{t("about.currentReleaseSub", { version: appVersion })}</p>
            </div>
          </div>
          {currentReleaseLoading ? (
            <div className="about-release-placeholder">{t("about.releaseLoading")}</div>
          ) : currentRelease?.body ? (
            <pre className="about-release-notes">{currentRelease.body}</pre>
          ) : (
            <div className="about-release-placeholder">{t("about.releaseEmpty")}</div>
          )}
          {currentReleaseError ? (
            <div className="inline-warning">{t("about.releaseLoadFailed", { error: currentReleaseError })}</div>
          ) : null}
        </div>
      </section>
    </div>
  );
}

/* ============================================================
   Request table (shared)
   ============================================================ */
export function RequestTable({
  requests,
  models,
  emptyTitle = "table.empty",
  emptyHint = "table.emptyHint",
}: {
  requests: AppSnapshot["requests"];
  models: ModelEntry[];
  emptyTitle?: MsgKey;
  emptyHint?: MsgKey;
}) {
  const { t } = useI18n();
  const [errorRecord, setErrorRecord] = React.useState<RequestRecordView | null>(null);
  const errorDetail = errorRecord ? requestErrorDetail(errorRecord, t, models) : "";
  const [imageData, setImageData] = React.useState<string | null>(null);
  async function openImagePreview(name: string) {
    setImageData("loading");
    try {
      const b64 = await api.readImagePreview(name);
      setImageData(b64 ? `data:image/png;base64,${b64}` : null);
    } catch {
      setImageData(null);
    }
  }
  async function copyErrorDetail() {
    if (!errorDetail) return;
    try {
      await navigator.clipboard.writeText(errorDetail);
    } catch {
      // Clipboard can be unavailable in some WebView contexts.
    }
  }
  if (requests.length === 0) {
    return <Empty icon={<Inbox size={26} />} title={t(emptyTitle)} hint={t(emptyHint)} />;
  }
  return (
    <>
      <div className="table-scroll">
        <div className="table">
          <div className="thead cols-req">
            <span>{t("table.time")}</span>
            <span>{t("table.model")}</span>
            <span>{t("table.providerProtocol")}</span>
            <span>{t("table.reasoning")}</span>
            <span>{t("table.tokens")}</span>
            <span className="hide-sm">{t("table.cost")}</span>
            <span>{t("table.status")}</span>
            <span className="req-stream-cell">{t("table.stream")}</span>
          </div>
          {requests.map((r) => {
            // TOKEN 列显示「清理前体积」(context_usage)；旧记录无体积时回退 usage。消费列用清理后 usage + cost。
            const vol = r.context_usage.total_tokens > 0 ? r.context_usage : r.usage;
            const billed = r.usage;
            const streamDisplay = streamStatusDisplay(r, t);
            const hasErrorDetail = Boolean(r.error || r.stream_error);
            return (
              <div className="trow cols-req" key={r.id}>
                <span className="mono">{new Date(r.started_at).toLocaleTimeString()}</span>
                <span className="model-cell">
                  <strong>{requestDisplayModel(r, models)}</strong>
                  {r.requested_model && r.requested_model !== r.model ? (
                    <span className="model-alias mono">{t("table.requestedModel", { model: r.requested_model })}</span>
                  ) : null}
                </span>
                <span className="provider-protocol-cell">
                  <strong>{r.provider_name ?? "—"}</strong>
                  <span>{r.provider_protocol ? t(protocolKey(r.provider_protocol)) : "—"}</span>
                </span>
                <span>
                  {r.reasoning_effort ? (
                    <span className="reasoning-cell">{REASONING_LABELS[r.reasoning_effort]}</span>
                  ) : (
                    <span style={{ color: "var(--faint)" }}>—</span>
                  )}
                </span>
                <span>
                  {r.image_preview ? (
                    <button
                      type="button"
                      className="img-preview-btn"
                      onClick={() => void openImagePreview(r.image_preview!)}
                      title={t("logs.imagePreview")}
                    >
                      <ImageIcon size={18} />
                    </button>
                  ) : vol.total_tokens > 0 ? (
                    <span className="tok-cell">
                      <strong>{formatTokens(vol.total_tokens)}</strong>
                      <span className="tok-mini">
                        ↑{formatTokens(vol.input_tokens)} ↓{formatTokens(vol.output_tokens)}
                        {vol.cache_read_tokens + vol.cache_write_tokens > 0
                          ? ` ⚡${formatTokens(vol.cache_read_tokens + vol.cache_write_tokens)}`
                          : ""}
                      </span>
                    </span>
                  ) : (
                    <span style={{ color: "var(--faint)" }}>—</span>
                  )}
                </span>
                <span className="hide-sm">
                  {r.cost_usd != null ? (
                    <span className="tok-cell">
                      <strong>{formatCost(r.cost_usd)}</strong>
                      <span className="tok-mini">{formatTokens(billed.total_tokens)}</span>
                    </span>
                  ) : (
                    <span style={{ color: "var(--faint)" }}>—</span>
                  )}
                </span>
                <span>
                  {hasErrorDetail ? (
                    <button
                      type="button"
                      className={`status-chip ${r.status < 400 ? "good" : "bad"} status-chip-button`}
                      onClick={() => setErrorRecord(r)}
                      title={t("logs.errorDetail")}
                    >
                      {r.status}
                    </button>
                  ) : (
                    <span className={`status-chip ${r.status < 400 ? "good" : "bad"}`}>{r.status}</span>
                  )}
                </span>
                <span className="req-stream-cell">
                  <span
                    className={`latency-chip ${streamDisplay.tone}`}
                    title={streamDisplay.title}
                  >
                    {streamDisplay.label}
                  </span>
                </span>
              </div>
            );
          })}
        </div>
      </div>
      <Modal
        open={Boolean(errorRecord)}
        onClose={() => setErrorRecord(null)}
        title={t("logs.errorDetail")}
        sub={errorRecord ? `${errorRecord.status} · ${requestDisplayModel(errorRecord, models)}` : ""}
        icon={<ListTree size={18} />}
        color="sakura"
        width={680}
        footer={
          <>
            <Button variant="ghost" onClick={() => setErrorRecord(null)}>{t("common.close")}</Button>
            <Button variant="primary" icon={<Copy size={16} />} onClick={() => void copyErrorDetail()}>{t("about.copy")}</Button>
          </>
        }
      >
        <pre className="error-detail-box">{errorDetail || "—"}</pre>
      </Modal>
      <Modal
        open={Boolean(imageData)}
        onClose={() => setImageData(null)}
        title={t("logs.imagePreview")}
        icon={<ImageIcon size={18} />}
        color="lav"
        width={560}
      >
        {imageData === "loading" ? (
          <div className="img-preview-loading">…</div>
        ) : imageData ? (
          <img src={imageData} alt="" className="img-preview-full" />
        ) : null}
      </Modal>
    </>
  );
}

/* ============================================================
   Settings modal
   ============================================================ */
export function SettingsModal({
  open,
  onClose,
  config,
  commit,
  refresh,
  notify,
  notifyRaw,
  busy,
}: {
  open: boolean;
  onClose: () => void;
  config: AppConfig;
  commit: PageProps["commit"];
  refresh: PageProps["refresh"];
  notify: PageProps["notify"];
  notifyRaw: PageProps["notifyRaw"];
  busy: boolean;
}) {
  const { t } = useI18n();
  const [host, setHost] = React.useState(config.settings.bind_host);
  const [port, setPort] = React.useState(config.settings.port);
  const [allowLan, setAllowLan] = React.useState(config.settings.allow_lan);
  const [lanKey, setLanKey] = React.useState(config.settings.lan_api_key);
  const [regeneratingLanKey, setRegeneratingLanKey] = React.useState(false);

  useSeedOnOpen(open, () => {
    setHost(config.settings.bind_host);
    setPort(config.settings.port);
    setAllowLan(config.settings.allow_lan);
    setLanKey(config.settings.lan_api_key);
  });

  async function submit() {
    const ok = await commit((d) => {
      d.settings.bind_host = host.trim();
      d.settings.port = Number(port);
      d.settings.allow_lan = allowLan;
      d.settings.lan_api_key = lanKey.trim();
    });
    if (ok) onClose();
  }

  async function regenerateLanKey() {
    setRegeneratingLanKey(true);
    try {
      const next = await api.regenerateLanApiKey();
      setLanKey(next.config.settings.lan_api_key);
      await refresh();
      notify("toast.lanKeyRegenerated");
    } catch (error) {
      notifyRaw(String(error), "bad");
    } finally {
      setRegeneratingLanKey(false);
    }
  }

  async function copyLanKey() {
    await navigator.clipboard.writeText(lanKey);
    notify("toast.copied");
  }

  function updateAllowLan(next: boolean) {
    setAllowLan(next);
    setHost(next ? "0.0.0.0" : "127.0.0.1");
  }

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={t("settings.title")}
      icon={<Settings2 size={18} />}
      color="mint"
      onEnter={submit}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>{t("common.cancel")}</Button>
          <Button variant="primary" onClick={submit} loading={busy}>{t("common.save")}</Button>
        </>
      }
    >
      <div className="grid grid-2">
        <Field label={t("settings.host")}>
          <Input value={host} autoFocus onChange={(e) => setHost(e.target.value)} />
        </Field>
        <Field label={t("settings.port")}>
          <Input type="number" value={port} onChange={(e) => setPort(Number(e.target.value))} />
        </Field>
      </div>
      <div className="modal-toggle-row">
        <div>
          <div className="mtr-title">{t("settings.allowLan")}</div>
          <div className="mtr-hint">{t("settings.allowLanHint")}</div>
        </div>
        <Switch checked={allowLan} onChange={updateAllowLan} />
      </div>
      {allowLan && (
        <Field label={t("settings.lanApiKey")}>
          <div className="input-action-row">
            <Input value={lanKey} onChange={(event) => setLanKey(event.target.value)} />
            <IconButton title={t("about.copy")} icon={<Copy size={16} />} onClick={copyLanKey} />
            <IconButton
              title={t("settings.regenerateLanApiKey")}
              icon={<RotateCcw size={16} className={regeneratingLanKey ? "spin" : ""} />}
              onClick={regenerateLanKey}
              disabled={regeneratingLanKey}
            />
          </div>
          <div className="field-hint">{t("settings.lanApiKeyHint")}</div>
        </Field>
      )}
    </Modal>
  );
}

/* ============================================================
   Dashboard
   ============================================================ */
type Range = "today" | "yesterday" | "last7";

function TokenBar({ label, value, total, cls }: { label: string; value: number; total: number; cls: string }) {
  const pct = total > 0 ? Math.round((value / total) * 100) : 0;
  return (
    <div className="tbar">
      <div className="tbar-head">
        <span className="tbar-label">{label}</span>
        <span className="tbar-value">{formatTokens(value)}</span>
      </div>
      <div className="tbar-track">
        <div className={`tbar-fill ${cls}`} style={{ width: `${pct}%` }} />
      </div>
    </div>
  );
}

export function Dashboard({ snapshot, config }: PageProps) {
  const { t } = useI18n();
  const [range, setRange] = React.useState<Range>("today");
  const visibleProviderIds = visibleUiProviderIds(config, snapshot);
  const enabledModels = config.models.filter((m) => m.enabled && visibleProviderIds.has(m.provider_id)).length;
  const providerCount = visibleProviderIds.size;
  const total = snapshot.requests.length;
  const success =
    total === 0
      ? 100
      : Math.round((snapshot.requests.filter((r) => r.status < 400).length / total) * 100);
  const avgLatency =
    total === 0
      ? 0
      : Math.round(snapshot.requests.reduce((a, r) => a + Number(r.latency_ms), 0) / total);
  const stats = snapshot.stats;
  const rangeTotals: TokenTotals = stats[range];
  const totalCost = snapshot.provider_usage.reduce(
    (sum, provider) => sum + (provider.local_usage.estimated_cost_usd ?? 0),
    0,
  );

  return (
    <div className="stack page-enter">
      <div className="grid grid-4">
        <Stat icon={<Coins size={15} />} label={t("dash.statTokens")} value={formatTokens(stats.all_time.total_tokens)} foot={t("dash.statTokensFoot", { requests: stats.all_time.requests })} grad />
        <Stat icon={<Coins size={15} />} label={t("dash.statTotalCost")} value={formatCost(totalCost)} foot={t("dash.statTotalCostFoot", { models: enabledModels })} grad />
        <Stat icon={<Server size={15} />} label={t("dash.statProviders")} value={providerCount} foot={t("dash.statProvidersFoot", { total: providerCount })} grad />
        <Stat icon={<Gauge size={15} />} label={t("dash.statSuccess")} value={`${success}%`} foot={total === 0 ? t("dash.successIdle") : t("dash.statSuccessFoot", { count: total, ms: avgLatency })} grad />
      </div>

      <Panel
        title={t("dash.tokensTitle")}
        sub={t("dash.tokensSub")}
        icon={<Coins size={18} />}
        color="peach"
        right={
          <div className="segmented">
            {(["today", "yesterday", "last7"] as Range[]).map((r) => (
              <button
                key={r}
                className={`seg ${range === r ? "active" : ""}`}
                onClick={() => setRange(r)}
              >
                {t(r === "today" ? "dash.range.today" : r === "yesterday" ? "dash.range.yesterday" : "dash.range.7d")}
              </button>
            ))}
          </div>
        }
      >
        <div className="token-panel">
          <div className="token-figures">
            <div className="token-big">
              <span className="tb-value">{formatTokens(rangeTotals.total_tokens)}</span>
              <span className="tb-label">{t("tokens.total")}</span>
            </div>
            <div className="token-breakdown">
              <TokenBar label={t("tokens.input")} value={rangeTotals.input_tokens} total={rangeTotals.total_tokens} cls="b-input" />
              <TokenBar label={t("tokens.output")} value={rangeTotals.output_tokens} total={rangeTotals.total_tokens} cls="b-output" />
              <TokenBar label={t("tokens.cacheRead")} value={rangeTotals.cache_read_tokens} total={rangeTotals.total_tokens} cls="b-cacheR" />
              <TokenBar label={t("tokens.cacheWrite")} value={rangeTotals.cache_write_tokens} total={rangeTotals.total_tokens} cls="b-cacheW" />
            </div>
          </div>
          <div className="token-chart">
            <div className="tc-title">{t("dash.trendTitle")}</div>
            <TrendChart data={stats.series} emptyLabel={t("chart.noData")} />
          </div>
        </div>
      </Panel>

      <Panel title={t("dash.recentTitle")} sub={t("dash.recentSub")} icon={<Activity size={18} />} color="lav">
        <RequestTable requests={snapshot.requests.slice(0, 6)} models={config.models} />
      </Panel>
    </div>
  );
}

/* ============================================================
   Model add/edit modal
   ============================================================ */
const REASONING_LABELS: Record<ReasoningEffort, string> = {
  low: "Low",
  medium: "Medium",
  high: "High",
  xhigh: "XHigh",
  max: "Max",
};

const MODEL_CONTEXT_WINDOWS = [128_000, 200_000, 258_000, 400_000, 1_000_000] as const;
const DEFAULT_MODEL_CONTEXT_WINDOW = 258_000;
const MODEL_CONTEXT_OPTIONS: Option[] = MODEL_CONTEXT_WINDOWS.map((value) => ({
  value: String(value),
  label: formatContext(value),
  tone: value >= 1_000_000 ? "bad" : value >= 400_000 ? "warn" : "ok",
}));

function normalizeModelContextWindow(value: number) {
  return MODEL_CONTEXT_WINDOWS.includes(value as (typeof MODEL_CONTEXT_WINDOWS)[number])
    ? value
    : DEFAULT_MODEL_CONTEXT_WINDOW;
}

function providerReasoningDefaults(provider: Provider | undefined) {
  return reasoningDefaultsForProtocol(provider?.protocol ?? "open_ai_responses");
}

function modelRuntimeDefaults(provider: Provider | undefined) {
  const reasoning = providerReasoningDefaults(provider);
  return {
    timeout_ms: 0,
    retry_count: 0,
    reasoning_enabled: reasoning.enabled && reasoning.levels.length > 0,
    default_reasoning_level: reasoning.defaultLevel,
    supported_reasoning_levels: reasoning.levels,
  };
}

function EMPTY_MODEL(provider: Provider | undefined): ModelEntry {
  return {
    // 模型 ID 不让用户填，底层自动随机。官方账号模式直接拿它作 Codex slug；
    // 第三方/局域网模式走 gpt 模型池(codex slot)，不受此 id 影响。
    id: `neko-model-${crypto.randomUUID().slice(0, 8)}`,
    display_name: "",
    description: "",
    context_window: DEFAULT_MODEL_CONTEXT_WINDOW,
    enabled: true,
    provider_id: provider?.id ?? "",
    upstream_model: null,
    codex_alias: null,
    image_generation: false,
    image_quality: null,
    ...modelRuntimeDefaults(provider),
  };
}

function modelIdKey(id: string) {
  return id.trim();
}

function sameModelId(a: string, b: string) {
  const left = modelIdKey(a);
  return left.length > 0 && left === modelIdKey(b);
}

function modelSwitchLabel(model: ModelEntry, config: AppConfig) {
  const provider = config.providers.find((p) => p.id === model.provider_id);
  const label = model.display_name.trim() || model.id.trim();
  return provider ? `${label} (${provider.name})` : label;
}

function ModelModal({
  open,
  onClose,
  snapshot,
  config,
  editIndex,
  commit,
  notifyRaw,
  busy,
}: {
  open: boolean;
  onClose: () => void;
  snapshot: AppSnapshot;
  config: AppConfig;
  editIndex: number | null;
  commit: PageProps["commit"];
  notifyRaw: PageProps["notifyRaw"];
  busy: boolean;
}) {
  const { t } = useI18n();
  const isEdit = editIndex !== null;
  const visibleProviders = React.useMemo(
    () => visibleUiProviders(config, snapshot),
    [config.providers, snapshot.keys],
  );
  const defaultProvider = visibleProviders[0] ?? config.providers[0];
  const [draft, setDraft] = React.useState<ModelEntry>(EMPTY_MODEL(defaultProvider));
  const [upstreamOptions, setUpstreamOptions] = React.useState<Option[]>([]);
  const [loadingUpstream, setLoadingUpstream] = React.useState(false);
  const [upstreamError, setUpstreamError] = React.useState<string | null>(null);
  const fetchToken = React.useRef(0);

  useSeedOnOpen(open, () => {
    if (isEdit && editIndex !== null) {
      const model = structuredClone(config.models[editIndex]);
      model.context_window = normalizeModelContextWindow(model.context_window);
      setDraft(model);
    } else {
      setDraft(EMPTY_MODEL(defaultProvider));
    }
  });

  // Fetch the selected provider's model catalog whenever it changes.
  React.useEffect(() => {
    if (!open || !draft.provider_id) {
      setUpstreamOptions([]);
      setLoadingUpstream(false);
      setUpstreamError(null);
      return;
    }
    const token = ++fetchToken.current;
    setLoadingUpstream(true);
    setUpstreamError(null);
    api
      .listUpstreamModels(draft.provider_id)
      .then((result) => {
        if (token !== fetchToken.current) return;
        setUpstreamOptions(result.models.map((m) => ({ value: m.id, label: m.label })));
        setUpstreamError(result.error ?? null);
      })
      .catch((error) => {
        if (token === fetchToken.current) {
          setUpstreamOptions([]);
          setUpstreamError(String(error));
        }
      })
      .finally(() => {
        if (token === fetchToken.current) setLoadingUpstream(false);
      });
  }, [open, draft.provider_id]);

  function patch(p: Partial<ModelEntry>) {
    setDraft((d) => ({ ...d, ...p }));
  }

  const selectedProvider = config.providers.find((p) => p.id === draft.provider_id);
  const protocolDefaults = providerReasoningDefaults(selectedProvider);
  const isImageModel = selectedProvider?.protocol === "open_ai_images";
  const draftModelId = modelIdKey(draft.id);
  const duplicateModels = config.models.filter((model, index) => {
    if (isEdit && editIndex === index) return false;
    return sameModelId(model.id, draftModelId);
  });
  const hasDuplicateModelId = duplicateModels.length > 0;
  const showContextPressureHint = normalizeModelContextWindow(draft.context_window) >= 400_000;

  function setProvider(providerId: string) {
    const provider = config.providers.find((p) => p.id === providerId);
    patch({
      provider_id: providerId,
      ...modelRuntimeDefaults(provider),
    });
  }

  const valid =
    draft.display_name.trim().length > 0 &&
    (draft.upstream_model ?? "").trim().length > 0 &&
    draft.provider_id.length > 0 &&
    visibleProviders.some((provider) => provider.id === draft.provider_id);

  async function submit() {
    if (!valid) return;
    const runtimeDefaults = {
      ...modelRuntimeDefaults(selectedProvider),
      default_reasoning_level: protocolDefaults.defaultLevel,
    };
    const clean: ModelEntry = {
      ...draft,
      id: draftModelId,
      display_name: draft.display_name.trim() || draftModelId,
      context_window: normalizeModelContextWindow(draft.context_window),
      upstream_model: draft.upstream_model?.trim() || null,
      ...runtimeDefaults,
      image_generation: isImageModel,
      image_quality: isImageModel ? draft.image_quality ?? "high" : null,
    };
    let addedAsDisabledDuplicate = false;
    if (!isEdit && hasDuplicateModelId) {
      clean.enabled = false;
      addedAsDisabledDuplicate = true;
    }
    const ok = await commit((d) => {
      if (isEdit && editIndex !== null) {
        d.models[editIndex] = clean;
        if (clean.enabled) {
          d.models.forEach((model, index) => {
            if (index !== editIndex && sameModelId(model.id, clean.id)) {
              model.enabled = false;
            }
          });
        }
      } else {
        d.models.push(clean);
      }
    }, addedAsDisabledDuplicate ? "toast.modelAddedDuplicateDisabled" : isEdit ? "toast.modelUpdated" : "toast.modelAdded");
    if (ok && isEdit && clean.enabled) {
      const switchedFrom = duplicateModels
        .filter((model) => model.enabled)
        .map((model) => modelSwitchLabel(model, config))
        .join(", ");
      if (switchedFrom) {
        notifyRaw(t("toast.modelDuplicateSwitched", { name: switchedFrom }));
      }
    }
    if (ok) onClose();
  }

  const providerOptions = visibleProviders.map((p) => ({
    value: p.id,
    label: p.name,
    sub: t(protocolKey(p.protocol)),
  }));

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={isEdit ? t("modelModal.editTitle") : t("modelModal.addTitle")}
      icon={<Cpu size={18} />}
      color="lav"
      width={580}
      onEnter={valid ? submit : undefined}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>{t("common.cancel")}</Button>
          <Button variant="primary" onClick={submit} loading={busy} disabled={!valid}>{t("common.save")}</Button>
        </>
      }
    >
      <Field label={t("model.displayName")}>
        <Input value={draft.display_name} autoFocus placeholder="GPT-5.5" onChange={(e) => patch({ display_name: e.target.value })} />
      </Field>
      <Field label={t("model.provider")}>
        <Dropdown value={draft.provider_id} options={providerOptions} onChange={setProvider} />
      </Field>
      <Field label={t("model.upstream")} hint={t("model.upstreamHint")}>
        <Combobox
          value={draft.upstream_model ?? ""}
          options={upstreamOptions}
          loading={loadingUpstream}
          placeholder={t("model.upstreamPlaceholder")}
          emptyHint={t("model.upstreamEmpty")}
          onChange={(v) => patch({ upstream_model: v || null })}
          onPick={(o) => {
            // 选上游自动带出显示名称(若未填)；模型 ID 保持自动随机，不跟随上游。
            const next: Partial<ModelEntry> = { upstream_model: o.value };
            if (!draft.display_name.trim()) next.display_name = o.label;
            patch(next);
          }}
        />
        {upstreamError ? (
          <div className="inline-warning">{t("model.upstreamError", { error: upstreamError })}</div>
        ) : null}
      </Field>
      {isImageModel ? (
        <Field label={t("model.imageQuality")}>
          <Dropdown
            value={draft.image_quality ?? "high"}
            options={[
              { value: "high", label: "High" },
              { value: "medium", label: "Medium" },
              { value: "low", label: "Low" },
            ]}
            onChange={(value) => patch({ image_quality: value })}
          />
        </Field>
      ) : (
        <>
          <Field label={t("model.context")}>
            <Dropdown
              value={String(normalizeModelContextWindow(draft.context_window))}
              options={MODEL_CONTEXT_OPTIONS}
              onChange={(value) => patch({ context_window: Number(value) })}
            />
          </Field>
          {showContextPressureHint ? <div className="inline-info">{t("model.contextPressureHint")}</div> : null}
          <Field label={t("model.description")}>
            <Input value={draft.description} onChange={(e) => patch({ description: e.target.value })} />
          </Field>
        </>
      )}
      <div className="modal-toggle-row">
        <div className="mtr-title">{t("common.enabled")}</div>
        <Switch checked={draft.enabled} onChange={(v) => patch({ enabled: v })} />
      </div>
    </Modal>
  );
}

/* ============================================================
   Test result modal
   ============================================================ */
const MODEL_TEST_OPTIONS: { value: ModelTestMode; key: MsgKey }[] = [
  { value: "connectivity", key: "test.modeConnectivity" },
  { value: "image", key: "test.modeImage" },
  { value: "context_400k", key: "test.mode400k" },
  { value: "context_1m", key: "test.mode1m" },
];

function TestModal({
  open,
  onClose,
  model,
  mode,
  onModeChange,
  onStart,
  onCancel,
  starting,
  status,
}: {
  open: boolean;
  onClose: () => void;
  model: string;
  mode: ModelTestMode;
  onModeChange: (mode: ModelTestMode) => void;
  onStart: () => void;
  onCancel: () => void;
  starting: boolean;
  status: ModelTestStatus | null;
}) {
  const { t } = useI18n();
  const running = starting || status?.state === "running";
  const r = status?.result ?? null;
  const targetLabel = status?.mode === "context_1m" ? "1M" : status?.mode === "context_400k" ? "400K" : "";
  const progress = status?.target_tokens
    ? Math.max(0, Math.min(100, (status.confirmed_tokens / status.pass_threshold_tokens) * 100))
    : 0;
  const modeOptions: Option[] = MODEL_TEST_OPTIONS.map((option) => ({
    value: option.value,
    label: t(option.key),
  }));
  return (
    <Modal
      open={open}
      onClose={onClose}
      title={t("test.title")}
      sub={model}
      icon={<Play size={18} />}
      color="sky"
      width={540}
      footer={
        <>
          {running ? (
            <Button variant="ghost" onClick={onCancel}>{t("common.cancel")}</Button>
          ) : (
            <Button variant="ghost" onClick={onClose}>{t("common.close")}</Button>
          )}
          <Button variant="primary" icon={<Play size={16} />} onClick={onStart} loading={starting} disabled={running}>
            {t("test.start")}
          </Button>
        </>
      }
    >
      <div className="stack">
        <Field label={t("test.mode")}>
          <Dropdown value={mode} options={modeOptions} onChange={(value) => onModeChange(value as ModelTestMode)} />
        </Field>

        {running ? (
          <div className="test-loading compact">
            <span className="test-spinner" />
            <p>{status?.mode === "image" ? t("test.stageImage") : status?.stage === "connectivity" ? t("test.stageConnectivity") : t("test.stageProbe")}</p>
          </div>
        ) : null}

        {status && status.mode !== "connectivity" && status.mode !== "image" ? (
          <div className="test-progress-box">
            <div className="test-progress-grid">
              <TestMetric label={t("test.currentContext")} value={formatContext(status.current_tokens)} muted={status.current_estimated ? t("test.estimated") : ""} />
              <TestMetric label={t("test.confirmedContext")} value={formatContext(status.confirmed_tokens)} muted={status.confirmed_estimated ? t("test.estimated") : ""} />
              <TestMetric label={t("test.targetContext")} value={targetLabel || "-"} />
              <TestMetric label={t("test.stage")} value={testStageLabel(t, status.stage)} />
            </div>
            {status.target_tokens ? (
              <div className="about-progress test-progress-bar">
                <span style={{ width: `${progress}%` }} />
              </div>
            ) : null}
          </div>
        ) : !status ? (
          <div className="test-idle">{t("test.pickMode")}</div>
        ) : null}

        {status?.state === "completed" || status?.state === "cancelled" ? (
          <div className={`test-summary ${status.supported ? "ok" : status.inconclusive ? "warn" : "bad"}`}>
            {testSummary(t, status)}
          </div>
        ) : null}

        {status?.last_error ? (
          <div className="inline-warning">{t("test.lastError", { error: status.last_error })}</div>
        ) : null}

        {r?.ok ? (
          <>
            <div className="test-reply">
              <div className="test-reply-label">{t("test.reply")} · {t("test.via", { provider: r.provider_name })}</div>
              <p>{r.reply || t("test.noReply")}</p>
              {r.image_preview ? (
                <img
                  src={`data:image/png;base64,${r.image_preview}`}
                  alt=""
                  className="img-preview-full"
                />
              ) : null}
            </div>
          </>
        ) : r ? (
          <div className="test-fail">
            <div className="test-fail-icon">!</div>
            <strong>{t("test.failed")}</strong>
            <p>{r.error === "needs_codex_auth" ? t("test.needsAuth") : r.error}</p>
          </div>
        ) : null}
      </div>
    </Modal>
  );
}

function TestMetric({ label, value, muted }: { label: string; value: string; muted?: string }) {
  return (
    <div className="test-metric">
      <span>{label}</span>
      <strong>{value}</strong>
      {muted ? <em>{muted}</em> : null}
    </div>
  );
}

function testStageLabel(t: (key: MsgKey, vars?: Record<string, string | number>) => string, stage: string) {
  switch (stage) {
    case "queued":
      return t("test.stageQueued");
    case "connectivity":
      return t("test.stageConnectivity");
    case "probe":
      return t("test.stageProbe");
    case "done":
      return t("test.stageDone");
    case "cancelled":
      return t("test.stageCancelled");
    default:
      return stage;
  }
}

function testSummary(t: (key: MsgKey, vars?: Record<string, string | number>) => string, status: ModelTestStatus) {
  if (status.state === "cancelled") return t("test.cancelled");
  if (status.mode === "connectivity") {
    return status.result?.ok ? t("test.connectivityOk") : t("test.failed");
  }
  if (status.mode === "image") {
    return status.result?.ok ? t("test.imageOk") : t("test.failed");
  }
  if (status.inconclusive) {
    return t("test.inconclusive", { error: status.last_error || status.summary || "" });
  }
  if (status.supported) {
    return status.mode === "context_1m" ? t("test.supported1m") : t("test.supported400k");
  }
  return status.mode === "context_1m"
    ? t("test.notReached1m", { tokens: formatContext(status.confirmed_tokens) })
    : t("test.notReached400k", { tokens: formatContext(status.confirmed_tokens) });
}

type DisplayModel = {
  model: ModelEntry;
  index: number;
  visualIndex: number;
};

function modelsForDisplay(models: ModelEntry[], visibleProviderIds: Set<string>): DisplayModel[] {
  return models
    .map((model, index) => ({ model, index }))
    .filter(({ model }) => visibleProviderIds.has(model.provider_id))
    .sort((a, b) => {
      if (a.model.enabled !== b.model.enabled) return a.model.enabled ? -1 : 1;
      return a.index - b.index;
    })
    .map((item, visualIndex) => ({ ...item, visualIndex }));
}

/* ============================================================
   Models page
   ============================================================ */
export function ModelGarden({ snapshot, config, commit, busy, notifyRaw }: PageProps) {
  const { t } = useI18n();
  const visibleProviderIds = React.useMemo(
    () => visibleUiProviderIds(config, snapshot),
    [config.providers, snapshot.keys],
  );
  const displayModels = React.useMemo(
    () => modelsForDisplay(config.models, visibleProviderIds),
    [config.models, visibleProviderIds],
  );
  const [modalOpen, setModalOpen] = React.useState(false);
  const [editIndex, setEditIndex] = React.useState<number | null>(null);
  const [deleteIndex, setDeleteIndex] = React.useState<number | null>(null);
  const [testModal, setTestModal] = React.useState<{ open: boolean; model: string; providerId: string }>({ open: false, model: "", providerId: "" });
  const [testMode, setTestMode] = React.useState<ModelTestMode>("connectivity");
  const [testStarting, setTestStarting] = React.useState(false);
  const [testStatus, setTestStatus] = React.useState<ModelTestStatus | null>(null);
  const [activeTestId, setActiveTestId] = React.useState<string | null>(null);
  const [dragIndex, setDragIndex] = React.useState<number | null>(null);
  const [dragOverIndex, setDragOverIndex] = React.useState<number | null>(null);
  const [dragPointer, setDragPointer] = React.useState<{ x: number; y: number } | null>(null);
  const [dragOffset, setDragOffset] = React.useState<{ x: number; y: number }>({ x: 35, y: 33 });
  const dragFromRef = React.useRef<number | null>(null);
  const dragOverRef = React.useRef<number | null>(null);
  const duplicateModelStats = React.useMemo(() => {
    const stats = new Map<string, { total: number; enabled: number }>();
    for (const { model } of displayModels) {
      const id = modelIdKey(model.id);
      if (!id) continue;
      const current = stats.get(id) ?? { total: 0, enabled: 0 };
      current.total += 1;
      if (model.enabled) current.enabled += 1;
      stats.set(id, current);
    }
    return stats;
  }, [displayModels]);
  const displayPositionByIndex = React.useMemo(() => {
    const positions = new Map<number, number>();
    displayModels.forEach((item) => positions.set(item.index, item.visualIndex));
    return positions;
  }, [displayModels]);

  function openAdd() {
    setEditIndex(null);
    setModalOpen(true);
  }
  function openEdit(i: number) {
    setEditIndex(i);
    setModalOpen(true);
  }

  async function confirmDelete() {
    if (deleteIndex === null) return;
    await commit((d) => {
      d.models.splice(deleteIndex, 1);
    }, "toast.modelDeleted");
    setDeleteIndex(null);
  }

  async function quickToggle(i: number, v: boolean) {
    const current = config.models[i];
    if (!current) return;
    const switchedFrom = v
      ? config.models
          .filter((model, index) => index !== i && model.enabled && sameModelId(model.id, current.id))
          .map((model) => modelSwitchLabel(model, config))
          .join(", ")
      : "";
    const ok = await commit((d) => {
      const target = d.models[i];
      if (!target) return;
      if (v) {
        d.models.forEach((model, index) => {
          if (index !== i && sameModelId(model.id, target.id)) {
            model.enabled = false;
          }
        });
        target.enabled = true;
      } else {
        target.enabled = false;
      }
    });
    if (ok && v && switchedFrom) {
      notifyRaw(t("toast.modelDuplicateSwitched", { name: switchedFrom }));
    }
  }

  async function reorderModels(from: number, to: number) {
    if (from === to || from < 0 || to < 0 || from >= config.models.length || to >= config.models.length) {
      return;
    }
    const ok = await commit((d) => {
      const [model] = d.models.splice(from, 1);
      if (!model) return;
      d.models.splice(to, 0, model);
    });
    if (ok && !config.settings.auto_inject) {
      try {
        await api.exportCatalog();
      } catch (error) {
        notifyRaw(String(error), "bad");
      }
    }
  }

  function setModelDragOver(index: number) {
    dragOverRef.current = index;
    setDragOverIndex((current) => (current === index ? current : index));
  }

  function clearModelDrag() {
    dragFromRef.current = null;
    dragOverRef.current = null;
    setDragIndex(null);
    setDragOverIndex(null);
    setDragPointer(null);
    setDragOffset({ x: 35, y: 33 });
    document.body.classList.remove("model-drag-active");
  }

  function beginModelDrag(index: number, event: React.PointerEvent<HTMLButtonElement>) {
    if (busy) return;
    event.preventDefault();
    const handleRect = event.currentTarget.getBoundingClientRect();
    const offsetX = 14 + ((event.clientX - handleRect.left) / Math.max(1, handleRect.width)) * 42;
    const offsetY = 12 + ((event.clientY - handleRect.top) / Math.max(1, handleRect.height)) * 42;
    dragFromRef.current = index;
    dragOverRef.current = index;
    setDragIndex(index);
    setDragOverIndex(index);
    setDragPointer({ x: event.clientX, y: event.clientY });
    setDragOffset({ x: offsetX, y: offsetY });
    document.body.classList.add("model-drag-active");

    const updateTarget = (clientX: number, clientY: number) => {
      const target = document.elementFromPoint(clientX, clientY);
      const row = target?.closest<HTMLElement>("[data-model-index]");
      const next = Number(row?.dataset.modelIndex);
      if (Number.isInteger(next) && next >= 0 && next < config.models.length) {
        setModelDragOver(next);
      }
    };

    const onMove = (pointerEvent: PointerEvent) => {
      pointerEvent.preventDefault();
      setDragPointer({ x: pointerEvent.clientX, y: pointerEvent.clientY });
      updateTarget(pointerEvent.clientX, pointerEvent.clientY);
    };
    const finish = () => {
      const from = dragFromRef.current;
      const to = dragOverRef.current;
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", finish);
      window.removeEventListener("pointercancel", cancel);
      clearModelDrag();
      if (from !== null && to !== null) void reorderModels(from, to);
    };
    const cancel = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", finish);
      window.removeEventListener("pointercancel", cancel);
      clearModelDrag();
    };

    window.addEventListener("pointermove", onMove, { passive: false });
    window.addEventListener("pointerup", finish);
    window.addEventListener("pointercancel", cancel);
  }

  async function runTest(modelId: string, providerId: string) {
    setTestModal({ open: true, model: modelId, providerId });
    setTestStatus(null);
    setActiveTestId(null);
  }

  async function startSelectedTest() {
    if (!testModal.model) return;
    setTestStarting(true);
    setTestStatus(null);
    try {
      const result = await api.startModelTest(testModal.model, testModal.providerId, testMode);
      setActiveTestId(result.test_id);
      const status = await api.getModelTestStatus(result.test_id);
      setTestStatus(status);
    } catch (error) {
      notifyRaw(String(error), "bad");
    } finally {
      setTestStarting(false);
    }
  }

  async function cancelSelectedTest() {
    if (!activeTestId) return;
    try {
      const status = await api.cancelModelTest(activeTestId);
      setTestStatus(status);
      setActiveTestId(null);
    } catch (error) {
      notifyRaw(String(error), "bad");
    }
  }

  async function closeTestModal() {
    if (testStatus?.state === "running" && activeTestId) {
      await cancelSelectedTest();
    }
    setTestModal({ open: false, model: "", providerId: "" });
    setTestStatus(null);
    setActiveTestId(null);
  }

  React.useEffect(() => {
    if (!activeTestId || !testModal.open) return;
    let stopped = false;
    const poll = async () => {
      try {
        const status = await api.getModelTestStatus(activeTestId);
        if (stopped) return;
        setTestStatus(status);
        if (status.state !== "running") {
          setActiveTestId(null);
        }
      } catch (error) {
        if (!stopped) {
          notifyRaw(String(error), "bad");
          setActiveTestId(null);
        }
      }
    };
    void poll();
    const timer = window.setInterval(() => void poll(), 1000);
    return () => {
      stopped = true;
      window.clearInterval(timer);
    };
  }, [activeTestId, notifyRaw, testModal.open]);

  const modelTokens = (id: string) =>
    snapshot.stats.by_model.find((m) => m.model === id)?.total_tokens ?? 0;
  const draggingModel = dragIndex !== null ? config.models[dragIndex] : null;
  const draggingProvider = draggingModel
    ? config.providers.find((p) => p.id === draggingModel.provider_id)
    : null;

  return (
    <div className="stack page-enter">
      <div className="row row-between wrap">
        <div className="page-lead">{t("models.count", { count: displayModels.length })}</div>
        <Button variant="primary" icon={<Plus size={16} />} onClick={openAdd}>{t("models.add")}</Button>
      </div>

      {displayModels.length === 0 ? (
        <Empty icon={<Cpu size={26} />} title={t("models.empty")} hint={t("models.emptyHint")} />
      ) : (
        <div className="entity-list">
          {displayModels.map(({ model, index, visualIndex }) => {
            const prov = config.providers.find((p) => p.id === model.provider_id);
            const ident = prov ? providerIcon(prov) : { icon: <CustomProviderIcon size={20} />, cls: "custom" };
            const tokens = modelTokens(model.id);
            const modelStats = duplicateModelStats.get(modelIdKey(model.id));
            const hasEnabledConflict = model.enabled && (modelStats?.enabled ?? 0) > 1;
            const dragVisualIndex = dragIndex === null ? null : displayPositionByIndex.get(dragIndex) ?? null;
            const dragOverVisualIndex = dragOverIndex === null ? null : displayPositionByIndex.get(dragOverIndex) ?? null;
            const shiftingDown =
              dragVisualIndex !== null &&
              dragOverVisualIndex !== null &&
              dragOverVisualIndex < dragVisualIndex &&
              visualIndex >= dragOverVisualIndex &&
              visualIndex < dragVisualIndex;
            const shiftingUp =
              dragVisualIndex !== null &&
              dragOverVisualIndex !== null &&
              dragOverVisualIndex > dragVisualIndex &&
              visualIndex > dragVisualIndex &&
              visualIndex <= dragOverVisualIndex;
            return (
              <div
                className={[
                  "entity-row model-row fade-up",
                  model.enabled ? "" : "off",
                  dragIndex === index ? "dragging" : "",
                  dragOverIndex === index && dragIndex !== index ? "drag-over" : "",
                  dragOverIndex === index && dragVisualIndex !== null && dragOverVisualIndex !== null && dragOverVisualIndex < dragVisualIndex ? "insert-before" : "",
                  dragOverIndex === index && dragVisualIndex !== null && dragOverVisualIndex !== null && dragOverVisualIndex > dragVisualIndex ? "insert-after" : "",
                  shiftingDown ? "drag-shift-down" : "",
                  shiftingUp ? "drag-shift-up" : "",
                ].filter(Boolean).join(" ")}
                key={`${model.provider_id}:${model.id}:${index}`}
                data-model-index={index}
                style={{ animationDelay: `${visualIndex * 0.04}s` }}
              >
                <button
                  type="button"
                  className="model-drag-handle"
                  title={t("model.dragHandle")}
                  aria-label={t("model.dragHandle")}
                  disabled={busy}
                  onPointerDown={(event) => beginModelDrag(index, event)}
                  onKeyDown={(event) => {
                    if (busy) return;
                    if (event.key === "ArrowUp" && visualIndex > 0) {
                      event.preventDefault();
                      void reorderModels(index, displayModels[visualIndex - 1]?.index ?? index);
                    }
                    if (event.key === "ArrowDown" && visualIndex < displayModels.length - 1) {
                      event.preventDefault();
                      void reorderModels(index, displayModels[visualIndex + 1]?.index ?? index);
                    }
                  }}
                >
                  <GripVertical size={18} />
                </button>
                <div className="entity-main">
                  <span className={`entity-avatar ${ident.cls}`}>{ident.icon}</span>
                  <div className="entity-title">
                    <strong>{model.display_name || t("models.empty")}</strong>
                    <span className="entity-sub mono">{model.id}</span>
                    <span className="entity-note">{prov?.name ?? "—"}</span>
                    {hasEnabledConflict ? (
                      <span className="entity-note warn">{t("model.duplicateEnabledConflict")}</span>
                    ) : null}
                  </div>
                </div>

                <div className="entity-meta">
                  <span className="meta-pair"><span>{t("model.context")}</span><strong>{formatContext(model.context_window)}</strong></span>
                  <span className="meta-pair"><span>{t("table.reasoning")}</span><strong>{model.reasoning_enabled ? REASONING_LABELS[model.default_reasoning_level] : "-"}</strong></span>
                  <span className="meta-pair"><span>{t("table.tokens")}</span><strong>{tokens > 0 ? formatTokens(tokens) : "-"}</strong></span>
                </div>

                <div className="entity-actions">
                  <Switch checked={model.enabled} onChange={(v) => quickToggle(index, v)} />
                  <Button variant="ghost" icon={<Play size={14} />} className="btn-sm" onClick={() => runTest(model.id, model.provider_id)}>{t("model.test")}</Button>
                  <IconButton icon={<Pencil size={15} />} title={t("common.edit")} onClick={() => openEdit(index)} />
                  <IconButton danger icon={<Trash2 size={15} />} title={t("common.delete")} onClick={() => setDeleteIndex(index)} />
                </div>
              </div>
            );
          })}
          {draggingModel && dragPointer && typeof document !== "undefined" ? createPortal(
            <div
              className="model-drag-preview"
              style={{ left: dragPointer.x - dragOffset.x, top: dragPointer.y - dragOffset.y }}
            >
              <span className="entity-avatar custom"><GripVertical size={18} /></span>
              <div className="entity-title">
                <strong>{draggingModel.display_name || draggingModel.id}</strong>
                <span className="entity-sub mono">{draggingModel.id}</span>
                <span className="entity-note">{draggingProvider?.name ?? "—"}</span>
              </div>
            </div>,
            document.body,
          ) : null}
        </div>
      )}

      <ModelModal open={modalOpen} onClose={() => setModalOpen(false)} snapshot={snapshot} config={config} editIndex={editIndex} commit={commit} notifyRaw={notifyRaw} busy={busy} />
      <TestModal
        open={testModal.open}
        onClose={() => void closeTestModal()}
        model={testModal.model}
        mode={testMode}
        onModeChange={setTestMode}
        onStart={() => void startSelectedTest()}
        onCancel={() => void cancelSelectedTest()}
        starting={testStarting}
        status={testStatus}
      />
      <ConfirmDialog
        open={deleteIndex !== null}
        onClose={() => setDeleteIndex(null)}
        onConfirm={confirmDelete}
        title={t("models.deleteTitle")}
        body={deleteIndex !== null ? t("models.deleteBody", { name: config.models[deleteIndex]?.display_name || config.models[deleteIndex]?.id || "" }) : ""}
        confirmLabel={t("common.delete")}
        icon={<Trash2 size={18} />}
        loading={busy}
      />
    </div>
  );
}

/* ============================================================
   Provider add/edit modal
   ============================================================ */
type ProviderFormKind = "custom" | "openai_account" | "claude_account";
type OpenAiAuthMode = "oauth" | "json";
type ClaudeAuthMode = "manual" | "cookie" | "json";

function providerProxyPasswordRef(providerId: string) {
  return `provider-proxy:${providerId}`;
}

function emptyHttpProxy(): ProviderHttpProxy {
  return {
    enabled: false,
    url: "",
    username: "",
    password_ref: null,
  };
}

function proxyForProvider(provider: Provider | null | undefined): ProviderHttpProxy {
  return provider?.http_proxy ?? emptyHttpProxy();
}

function normalizeProxyInput(raw: string, username: string, password: string) {
  let input = raw.trim();
  let nextUsername = username.trim();
  let nextPassword = password;
  if (!input) {
    return { url: "", username: nextUsername, password: nextPassword };
  }
  if (!/^https?:\/\//i.test(input)) input = `http://${input}`;
  const url = new URL(input);
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new Error("HTTP proxy must start with http:// or https://");
  }
  if (!url.hostname) {
    throw new Error("HTTP proxy host is required");
  }
  if (url.username && !nextUsername) {
    nextUsername = decodeURIComponent(url.username);
  }
  if (url.password) {
    nextPassword = decodeURIComponent(url.password);
  }
  url.username = "";
  url.password = "";
  const clean = url.toString().replace(/\/$/, "");
  return { url: clean, username: nextUsername, password: nextPassword };
}

function providerFormKind(provider: Provider | null): ProviderFormKind {
  if (provider?.kind === "official_open_ai_account") return "openai_account";
  if (provider?.kind === "official_anthropic_account") return "claude_account";
  return "custom";
}

function ProviderModal({
  open,
  onClose,
  config,
  editId,
  commit,
  refresh,
  setBusy,
  busy,
  notify,
  notifyRaw,
}: {
  open: boolean;
  onClose: () => void;
  config: AppConfig;
  editId: string | null;
  commit: PageProps["commit"];
  refresh: PageProps["refresh"];
  setBusy: PageProps["setBusy"];
  busy: boolean;
  notify: PageProps["notify"];
  notifyRaw: PageProps["notifyRaw"];
}) {
  const { t } = useI18n();
  const isEdit = editId !== null;
  const existing = isEdit ? config.providers.find((p) => p.id === editId) ?? null : null;

  const [formKind, setFormKind] = React.useState<ProviderFormKind>("custom");
  const [name, setName] = React.useState("");
  const [protocol, setProtocol] = React.useState<ProviderProtocol>("open_ai_responses");
  const [baseUrl, setBaseUrl] = React.useState("");
  const [useKey, setUseKey] = React.useState(true);
  const [secret, setSecret] = React.useState("");
  const [showSecret, setShowSecret] = React.useState(false);
  const [tokenJson, setTokenJson] = React.useState("");
  const [openAiAuthMode, setOpenAiAuthMode] = React.useState<OpenAiAuthMode>("oauth");
  const [claudeAuthMode, setClaudeAuthMode] = React.useState<ClaudeAuthMode>("manual");
  const [oauthSessionId, setOauthSessionId] = React.useState("");
  const [oauthAuthUrl, setOauthAuthUrl] = React.useState("");
  const [oauthCallback, setOauthCallback] = React.useState("");
  const [claudeSessionKey, setClaudeSessionKey] = React.useState("");
  const [oauthLoading, setOauthLoading] = React.useState(false);
  const [proxyEnabled, setProxyEnabled] = React.useState(false);
  const [proxyUrl, setProxyUrl] = React.useState("");
  const [proxyUsername, setProxyUsername] = React.useState("");
  const [proxyPassword, setProxyPassword] = React.useState("");
  const [credentialLoaded, setCredentialLoaded] = React.useState(false);
  const [proxyPasswordLoaded, setProxyPasswordLoaded] = React.useState(false);

  useSeedOnOpen(open, () => {
    if (existing) {
      const proxy = proxyForProvider(existing);
      setFormKind(providerFormKind(existing));
      setName(existing.name);
      setProtocol(existing.protocol);
      setBaseUrl(existing.base_url);
      setUseKey(Boolean(existing.key_ref));
      setOpenAiAuthMode(existing.kind === "official_open_ai_account" ? "json" : "oauth");
      setClaudeAuthMode(existing.kind === "official_anthropic_account" ? "json" : "manual");
      setProxyEnabled(proxy.enabled);
      setProxyUrl(proxy.url);
      setProxyUsername(proxy.username);
      setCredentialLoaded(false);
      setProxyPasswordLoaded(false);
    } else {
      const seed = newCustomProvider();
      setFormKind("custom");
      setName(seed.name);
      setProtocol(seed.protocol);
      setBaseUrl(seed.base_url);
      setUseKey(true);
      setOpenAiAuthMode("oauth");
      setClaudeAuthMode("manual");
      setProxyEnabled(false);
      setProxyUrl("");
      setProxyUsername("");
      setCredentialLoaded(true);
      setProxyPasswordLoaded(true);
    }
    setSecret("");
    setShowSecret(false);
    setTokenJson("");
    setProxyPassword("");
    setOauthSessionId("");
    setOauthAuthUrl("");
    setOauthCallback("");
    setClaudeSessionKey("");
  });

  React.useEffect(() => {
    if (!open || !existing) return;
    let cancelled = false;
    api
      .readProviderCredential(existing.id)
      .then((credential) => {
        if (cancelled) return;
        if (existing.kind === "custom") {
          setSecret(credential.value);
        } else if (isOfficialAccountProvider(existing)) {
          setTokenJson(credential.value);
        }
        setCredentialLoaded(true);
      })
      .catch((error) => {
        if (!cancelled) notifyRaw(String(error), "bad");
        if (!cancelled) setCredentialLoaded(true);
      });
    api
      .readProviderProxyPassword(existing.id)
      .then((password) => {
        if (!cancelled) {
          setProxyPassword(password);
          setProxyPasswordLoaded(true);
        }
      })
      .catch((error) => {
        if (!cancelled) notifyRaw(String(error), "bad");
        if (!cancelled) setProxyPasswordLoaded(true);
      });
    return () => {
      cancelled = true;
    };
  }, [open, existing?.id]);

  const officialAccount = formKind !== "custom";
  const openAiAccount = formKind === "openai_account";
  const claudeAccount = formKind === "claude_account";
  const openAiOAuthMode = openAiAccount && openAiAuthMode === "oauth";
  const claudeManualMode = claudeAccount && claudeAuthMode === "manual";
  const claudeCookieMode = claudeAccount && claudeAuthMode === "cookie";
  const officialJsonMode = (openAiAccount && openAiAuthMode === "json") || (claudeAccount && claudeAuthMode === "json");
  const officialCredentialReady = openAiOAuthMode || claudeManualMode
    ? oauthCallback.trim().length > 0
    : claudeCookieMode
      ? claudeSessionKey.trim().length > 0
      : tokenJson.trim().length > 0;
  const editSecretsLoaded = !isEdit || (credentialLoaded && proxyPasswordLoaded);
  const proxyValid = !proxyEnabled || proxyUrl.trim().length > 0;
  const valid = editSecretsLoaded && proxyValid && name.trim().length > 0 && (officialAccount ? isEdit || officialCredentialReady : baseUrl.trim().length > 0);

  const protoOptions = [
    { value: "open_ai_responses", label: t("proto.responses") },
    { value: "open_ai_chat_completions", label: t("proto.chat") },
    { value: "anthropic_messages", label: t("proto.anthropic") },
    { value: "open_ai_images", label: t("proto.images") },
  ];
  const providerTypeOptions = [
    { value: "custom", label: t("providerType.custom") },
    { value: "openai_account", label: t("providerType.openaiAccount") },
    { value: "claude_account", label: t("providerType.claudeAccount") },
  ];

  function switchProviderType(next: string) {
    const kind = next as ProviderFormKind;
    setFormKind(kind);
    if (kind === "custom") {
      const seed = newCustomProvider();
      setName(seed.name);
      setProtocol(seed.protocol);
      setBaseUrl(seed.base_url);
      setUseKey(true);
      setShowSecret(false);
      setProxyEnabled(false);
      setProxyUrl("");
      setProxyUsername("");
      setProxyPassword("");
    } else if (kind === "openai_account") {
      const seed = newOpenAiAccountProvider();
      setName(seed.name);
      setProtocol(seed.protocol);
      setBaseUrl(seed.base_url);
      setUseKey(false);
      setShowSecret(false);
      setOpenAiAuthMode("oauth");
      setClaudeAuthMode("manual");
      setProxyEnabled(false);
      setProxyUrl("");
      setProxyUsername("");
      setProxyPassword("");
    } else {
      const seed = newClaudeAccountProvider();
      setName(seed.name);
      setProtocol(seed.protocol);
      setBaseUrl(seed.base_url);
      setUseKey(false);
      setShowSecret(false);
      setOpenAiAuthMode("oauth");
      setClaudeAuthMode("manual");
      setProxyEnabled(false);
      setProxyUrl("");
      setProxyUsername("");
      setProxyPassword("");
    }
  }

  async function generateAuthLink(kind: "openai" | "claude") {
    setOauthLoading(true);
    try {
      const session = kind === "openai" ? await api.startOpenAiOAuth() : await api.startClaudeOAuth();
      setOauthSessionId(session.session_id);
      setOauthAuthUrl(session.auth_url);
      setOauthCallback("");
      notify("provider.authLinkReady");
    } catch (error) {
      notifyRaw(String(error), "bad");
    } finally {
      setOauthLoading(false);
    }
  }

  async function openAuthLink() {
    if (!oauthAuthUrl) return;
    try {
      await openUrl(oauthAuthUrl);
    } catch (error) {
      notifyRaw(String(error), "bad");
    }
  }

  async function copyAuthLink() {
    if (!oauthAuthUrl) return;
    try {
      await navigator.clipboard.writeText(oauthAuthUrl);
      notify("toast.copied");
    } catch (error) {
      notifyRaw(String(error), "bad");
    }
  }

  async function copyText(value: string) {
    if (!value) return;
    try {
      await navigator.clipboard.writeText(value);
      notify("toast.copied");
    } catch (error) {
      notifyRaw(String(error), "bad");
    }
  }

  async function submit() {
    if (!valid) return;
    if ((openAiOAuthMode || claudeManualMode) && oauthCallback.trim() && !oauthSessionId) {
      notify("provider.oauthSessionMissing", "bad");
      return;
    }
    if (officialJsonMode && tokenJson.trim()) {
      try {
        JSON.parse(tokenJson);
      } catch (error) {
        notifyRaw(String(error), "bad");
        return;
      }
    }
    let parsedProxy: { url: string; username: string; password: string };
    try {
      parsedProxy = normalizeProxyInput(proxyUrl, proxyUsername, proxyPassword);
    } catch (error) {
      notifyRaw(String(error), "bad");
      return;
    }
    if (proxyEnabled && !parsedProxy.url) {
      notifyRaw(t("provider.proxyRequired"), "bad");
      return;
    }
    const proxyForId = (id: string): ProviderHttpProxy => proxyEnabled
      ? {
          enabled: true,
          url: parsedProxy.url,
          username: parsedProxy.username,
          password_ref: parsedProxy.password ? providerProxyPasswordRef(id) : null,
        }
      : emptyHttpProxy();
    let providerId = editId;
    const cleanUrl = officialAccount ? baseUrl : normalizeBaseUrl(baseUrl);
    const ok = await commit((d) => {
      if (existing) {
        const idx = d.providers.findIndex((p) => p.id === existing.id);
        if (idx >= 0) {
          d.providers[idx].name = name.trim();
          d.providers[idx].http_proxy = proxyForId(existing.id);
          if (existing.kind === "custom") {
            d.providers[idx].protocol = protocol;
            d.providers[idx].base_url = cleanUrl;
            d.providers[idx].key_ref = useKey ? `provider:${existing.id}` : null;
          }
          if (existing.kind === "official_open_ai_account") {
            d.providers[idx].protocol = "open_ai_responses";
            d.providers[idx].base_url = "https://api.openai.com/v1";
            d.providers[idx].key_ref = `official-token:${existing.id}`;
          }
          if (existing.kind === "official_anthropic_account") {
            d.providers[idx].protocol = "anthropic_messages";
            d.providers[idx].base_url = "https://api.anthropic.com/v1";
            d.providers[idx].key_ref = `official-token:${existing.id}`;
          }
        }
      } else if (formKind === "openai_account") {
        const seed = newOpenAiAccountProvider();
        providerId = seed.id;
        d.providers.push({ ...seed, name: name.trim(), http_proxy: proxyForId(seed.id) });
      } else if (formKind === "claude_account") {
        const seed = newClaudeAccountProvider();
        providerId = seed.id;
        d.providers.push({ ...seed, name: name.trim(), http_proxy: proxyForId(seed.id) });
      } else {
        const seed = newCustomProvider();
        providerId = seed.id;
        d.providers.push({
          ...seed,
          name: name.trim(),
          protocol,
          base_url: cleanUrl,
          key_ref: useKey ? seed.key_ref : null,
          http_proxy: proxyForId(seed.id),
        });
      }
    }, isEdit ? "toast.providerUpdated" : "toast.providerAdded");

    if (!ok) return;

    if (providerId) {
      setBusy(true);
      try {
        if (proxyEnabled && parsedProxy.password) {
          await api.setProviderProxyPassword(providerId, parsedProxy.password);
        } else {
          await api.deleteProviderProxyPassword(providerId);
        }
      } catch (error) {
        notifyRaw(String(error), "bad");
        return;
      } finally {
        setBusy(false);
      }
    }

    if (openAiOAuthMode && oauthCallback.trim() && providerId) {
      setBusy(true);
      try {
        await api.finishOpenAiOAuth(providerId, oauthSessionId, oauthCallback.trim());
        notify("toast.keySaved");
        await refresh();
      } catch (error) {
        notifyRaw(String(error), "bad");
        return;
      } finally {
        setBusy(false);
      }
    } else if (claudeManualMode && oauthCallback.trim() && providerId) {
      setBusy(true);
      try {
        await api.finishClaudeOAuth(providerId, oauthSessionId, oauthCallback.trim());
        notify("toast.keySaved");
        await refresh();
      } catch (error) {
        notifyRaw(String(error), "bad");
        return;
      } finally {
        setBusy(false);
      }
    } else if (claudeCookieMode && claudeSessionKey.trim() && providerId) {
      setBusy(true);
      try {
        await api.finishClaudeCookieOAuth(providerId, claudeSessionKey.trim());
        notify("toast.keySaved");
        await refresh();
      } catch (error) {
        notifyRaw(String(error), "bad");
        return;
      } finally {
        setBusy(false);
      }
    } else if (officialJsonMode && tokenJson.trim() && providerId) {
      setBusy(true);
      try {
        await api.setOfficialProviderToken(providerId, tokenJson.trim());
        notify("toast.keySaved");
        await refresh();
      } catch (error) {
        notifyRaw(String(error), "bad");
        return;
      } finally {
        setBusy(false);
      }
    } else if (officialJsonMode && isEdit && providerId) {
      setBusy(true);
      try {
        await api.deleteOfficialProviderToken(providerId);
        notify("toast.keyDeleted");
        await refresh();
      } catch (error) {
        notifyRaw(String(error), "bad");
        return;
      } finally {
        setBusy(false);
      }
    } else if (!officialAccount && useKey && providerId) {
      setBusy(true);
      try {
        if (secret.trim()) {
          await api.setProviderKey(providerId, secret.trim());
          notify("toast.keySaved");
        } else {
          await api.deleteProviderKey(providerId);
          notify("toast.keyDeleted");
        }
        await refresh();
      } catch (error) {
        notifyRaw(String(error), "bad");
        return;
      } finally {
        setBusy(false);
      }
    }
    onClose();
  }

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={isEdit ? t("providerModal.editTitle") : t("providerModal.addTitle")}
      icon={<CustomProviderIcon size={18} />}
      color="sakura"
      width={officialAccount ? 680 : 560}
      onEnter={valid ? submit : undefined}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>{t("common.cancel")}</Button>
          <Button variant="primary" onClick={submit} loading={busy} disabled={!valid}>{t("common.save")}</Button>
        </>
      }
    >
      {!isEdit ? (
        <Field label={t("provider.type")}>
          <Dropdown value={formKind} options={providerTypeOptions} onChange={switchProviderType} />
        </Field>
      ) : null}
      <Field label={t("provider.name")}>
        <Input value={name} autoFocus onChange={(e) => setName(e.target.value)} />
      </Field>
      {openAiAccount ? (
        <>
          <Field label={t("provider.authMethod")}>
            <div className="segmented provider-auth-mode">
              {(["oauth", "json"] as OpenAiAuthMode[]).map((mode) => (
                <button
                  key={mode}
                  className={`seg ${openAiAuthMode === mode ? "active" : ""}`}
                  onClick={() => setOpenAiAuthMode(mode)}
                  type="button"
                >
                  {t(mode === "oauth" ? "provider.openAiOAuth" : "provider.codexJson")}
                </button>
              ))}
            </div>
          </Field>
          {openAiAuthMode === "oauth" ? (
            <div className="oauth-flow-box">
              <div className="row wrap">
                <Button
                  variant="primary"
                  icon={<ExternalLink size={16} />}
                  onClick={() => generateAuthLink("openai")}
                  loading={oauthLoading}
                >
                  {t("provider.generateAuthLink")}
                </Button>
                <Button
                  variant="ghost"
                  icon={<ExternalLink size={16} />}
                  onClick={openAuthLink}
                  disabled={!oauthAuthUrl}
                >
                  {t("provider.openAuthLink")}
                </Button>
                <Button
                  variant="ghost"
                  icon={<Copy size={16} />}
                  onClick={copyAuthLink}
                  disabled={!oauthAuthUrl}
                >
                  {t("provider.copyAuthLink")}
                </Button>
              </div>
              {oauthAuthUrl ? (
                <Input value={oauthAuthUrl} readOnly />
              ) : null}
              <Field label={t("provider.oauthCallback")} hint={t("provider.oauthHint")}>
                <textarea
                  className="input oauth-code-input"
                  value={oauthCallback}
                  placeholder={t("provider.oauthCallbackPlaceholder")}
                  onChange={(event) => setOauthCallback(event.target.value)}
                />
              </Field>
            </div>
          ) : (
            <Field label={t("provider.tokenJson")} hint={isEdit ? t("provider.tokenJsonEditHint") : t("provider.tokenJsonHint")}>
              <div className="row row-end">
                <Button variant="ghost" icon={<Copy size={16} />} onClick={() => copyText(tokenJson)} disabled={!tokenJson}>
                  {t("common.copy")}
                </Button>
              </div>
              <textarea
                className="input token-json-input"
                value={tokenJson}
                placeholder={'{\n  "access_token": "...",\n  "refresh_token": "...",\n  "expires_at": "2099-01-01T00:00:00Z"\n}'}
                onChange={(event) => setTokenJson(event.target.value)}
              />
            </Field>
          )}
        </>
      ) : claudeAccount ? (
        <>
          <Field label={t("provider.authMethod")}>
            <div className="segmented provider-auth-mode">
              {(["manual", "cookie", "json"] as ClaudeAuthMode[]).map((mode) => (
                <button
                  key={mode}
                  className={`seg ${claudeAuthMode === mode ? "active" : ""}`}
                  onClick={() => {
                    setClaudeAuthMode(mode);
                    setOauthSessionId("");
                    setOauthAuthUrl("");
                    setOauthCallback("");
                    setClaudeSessionKey("");
                  }}
                  type="button"
                >
                  {t(
                    mode === "manual"
                      ? "provider.claudeManualAuth"
                      : mode === "cookie"
                        ? "provider.claudeCookieAuth"
                        : "provider.claudeJson",
                  )}
                </button>
              ))}
            </div>
          </Field>
          {claudeAuthMode === "manual" ? (
            <div className="oauth-flow-box">
              <div className="row wrap">
                <Button
                  variant="primary"
                  icon={<ExternalLink size={16} />}
                  onClick={() => generateAuthLink("claude")}
                  loading={oauthLoading}
                >
                  {t("provider.generateAuthLink")}
                </Button>
                <Button
                  variant="ghost"
                  icon={<ExternalLink size={16} />}
                  onClick={openAuthLink}
                  disabled={!oauthAuthUrl}
                >
                  {t("provider.openAuthLink")}
                </Button>
                <Button
                  variant="ghost"
                  icon={<Copy size={16} />}
                  onClick={copyAuthLink}
                  disabled={!oauthAuthUrl}
                >
                  {t("provider.copyAuthLink")}
                </Button>
              </div>
              {oauthAuthUrl ? (
                <Input value={oauthAuthUrl} readOnly />
              ) : null}
              <Field label={t("provider.oauthCallback")} hint={t("provider.claudeOauthHint")}>
                <textarea
                  className="input oauth-code-input"
                  value={oauthCallback}
                  placeholder={t("provider.claudeOauthCallbackPlaceholder")}
                  onChange={(event) => setOauthCallback(event.target.value)}
                />
              </Field>
            </div>
          ) : claudeAuthMode === "cookie" ? (
            <div className="oauth-flow-box">
              <Field label={t("provider.claudeSessionKey")} hint={t("provider.claudeSessionKeyHint")}>
                <textarea
                  className="input oauth-code-input"
                  value={claudeSessionKey}
                  placeholder={t("provider.claudeSessionKeyPlaceholder")}
                  onChange={(event) => setClaudeSessionKey(event.target.value)}
                />
              </Field>
              <ol className="oauth-help-list">
                <li>{t("provider.claudeCookieStep1")}</li>
                <li>{t("provider.claudeCookieStep2")}</li>
                <li>{t("provider.claudeCookieStep3")}</li>
              </ol>
            </div>
          ) : (
            <Field label={t("provider.tokenJson")} hint={isEdit ? t("provider.tokenJsonEditHint") : t("provider.tokenJsonHint")}>
              <div className="row row-end">
                <Button variant="ghost" icon={<Copy size={16} />} onClick={() => copyText(tokenJson)} disabled={!tokenJson}>
                  {t("common.copy")}
                </Button>
              </div>
              <textarea
                className="input token-json-input"
                value={tokenJson}
                placeholder={'{\n  "access_token": "...",\n  "refresh_token": "...",\n  "expires_at": "2099-01-01T00:00:00Z"\n}'}
                onChange={(event) => setTokenJson(event.target.value)}
              />
            </Field>
          )}
        </>
      ) : officialAccount ? (
        <Field label={t("provider.tokenJson")} hint={isEdit ? t("provider.tokenJsonEditHint") : t("provider.tokenJsonHint")}>
          <div className="row row-end">
            <Button variant="ghost" icon={<Copy size={16} />} onClick={() => copyText(tokenJson)} disabled={!tokenJson}>
              {t("common.copy")}
            </Button>
          </div>
          <textarea
            className="input token-json-input"
            value={tokenJson}
            placeholder={'{\n  "access_token": "...",\n  "refresh_token": "...",\n  "expires_at": "2099-01-01T00:00:00Z"\n}'}
            onChange={(event) => setTokenJson(event.target.value)}
          />
        </Field>
      ) : (
        <>
          <Field label={t("provider.protocol")}>
            <Dropdown value={protocol} options={protoOptions} onChange={(v) => setProtocol(v as ProviderProtocol)} />
          </Field>
          <Field label={t("provider.apiAddress")}>
            <Input value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} />
          </Field>
        </>
      )}
      {!officialAccount ? (
        <div className="modal-toggle-row">
          <div className="mtr-title">{t("provider.useKey")}</div>
          <Switch checked={useKey} onChange={setUseKey} />
        </div>
      ) : null}
      {!officialAccount && useKey ? (
        <Field label={t("provider.apiKey")}>
          <div className="input-action-row">
            <Input
              type={showSecret ? "text" : "password"}
              value={secret}
              placeholder={t("provider.apiKeyPlaceholder")}
              autoComplete="off"
              onChange={(e) => setSecret(e.target.value)}
            />
            <IconButton
              icon={showSecret ? <EyeOff size={16} /> : <Eye size={16} />}
              title={t(showSecret ? "provider.hideSecret" : "provider.showSecret")}
              onClick={() => setShowSecret((value) => !value)}
            />
            <IconButton
              icon={<Copy size={16} />}
              title={t("common.copy")}
              onClick={() => copyText(secret)}
              disabled={!secret}
            />
          </div>
        </Field>
      ) : null}
      <div className="modal-toggle-row">
        <div>
          <div className="mtr-title">{t("provider.httpProxy")}</div>
          <div className="mtr-hint">{t("provider.httpProxyHint")}</div>
        </div>
        <Switch checked={proxyEnabled} onChange={setProxyEnabled} />
      </div>
      {proxyEnabled ? (
        <>
          <Field label={t("provider.proxyUrl")} hint={t("provider.proxyUrlHint")}>
            <Input
              value={proxyUrl}
              placeholder="http://127.0.0.1:7890"
              onChange={(event) => setProxyUrl(event.target.value)}
            />
          </Field>
          <Field label={t("provider.proxyUsername")}>
            <Input
              value={proxyUsername}
              onChange={(event) => setProxyUsername(event.target.value)}
            />
          </Field>
          <Field label={t("provider.proxyPassword")}>
            <Input
              value={proxyPassword}
              onChange={(event) => setProxyPassword(event.target.value)}
            />
          </Field>
        </>
      ) : null}
    </Modal>
  );
}

function ProviderProxyModal({
  open,
  onClose,
  provider,
  commit,
  refresh,
  setBusy,
  busy,
  notify,
  notifyRaw,
}: {
  open: boolean;
  onClose: () => void;
  provider: Provider | null;
  commit: PageProps["commit"];
  refresh: PageProps["refresh"];
  setBusy: PageProps["setBusy"];
  busy: boolean;
  notify: PageProps["notify"];
  notifyRaw: PageProps["notifyRaw"];
}) {
  const { t } = useI18n();
  const [enabled, setEnabled] = React.useState(false);
  const [url, setUrl] = React.useState("");
  const [username, setUsername] = React.useState("");
  const [password, setPassword] = React.useState("");
  const [loaded, setLoaded] = React.useState(false);

  useSeedOnOpen(open, () => {
    const proxy = proxyForProvider(provider);
    setEnabled(proxy.enabled);
    setUrl(proxy.url);
    setUsername(proxy.username);
    setPassword("");
    setLoaded(false);
  });

  React.useEffect(() => {
    if (!open || !provider) return;
    let cancelled = false;
    api
      .readProviderProxyPassword(provider.id)
      .then((value) => {
        if (!cancelled) {
          setPassword(value);
          setLoaded(true);
        }
      })
      .catch((error) => {
        if (!cancelled) notifyRaw(String(error), "bad");
        if (!cancelled) setLoaded(true);
      });
    return () => {
      cancelled = true;
    };
  }, [open, provider?.id]);

  if (!provider) return null;

  async function save() {
    if (!provider) return;
    let parsed: { url: string; username: string; password: string };
    try {
      parsed = normalizeProxyInput(url, username, password);
    } catch (error) {
      notifyRaw(String(error), "bad");
      return;
    }
    if (enabled && !parsed.url) {
      notifyRaw(t("provider.proxyRequired"), "bad");
      return;
    }
    const nextProxy: ProviderHttpProxy = enabled
      ? {
          enabled: true,
          url: parsed.url,
          username: parsed.username,
          password_ref: parsed.password ? providerProxyPasswordRef(provider.id) : null,
        }
      : emptyHttpProxy();
    const ok = await commit((draft) => {
      const item = draft.providers.find((candidate) => candidate.id === provider.id);
      if (item) item.http_proxy = nextProxy;
    }, "toast.providerUpdated");
    if (!ok) return;

    setBusy(true);
    try {
      if (enabled && parsed.password) {
        await api.setProviderProxyPassword(provider.id, parsed.password);
      } else {
        await api.deleteProviderProxyPassword(provider.id);
      }
      await refresh();
      onClose();
    } catch (error) {
      notifyRaw(String(error), "bad");
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={t("provider.proxySettings")}
      sub={provider.name}
      icon={<Settings2 size={18} />}
      color="sakura"
      width={560}
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>{t("common.cancel")}</Button>
          <Button variant="primary" onClick={save} loading={busy} disabled={!loaded || (enabled && !url.trim())}>{t("common.save")}</Button>
        </>
      }
    >
      <div className="modal-toggle-row">
        <div>
          <div className="mtr-title">{t("provider.httpProxy")}</div>
          <div className="mtr-hint">{t("provider.httpProxyHint")}</div>
        </div>
        <Switch checked={enabled} onChange={setEnabled} />
      </div>
      {enabled ? (
        <>
          <Field label={t("provider.proxyUrl")} hint={t("provider.proxyUrlHint")}>
            <Input value={url} placeholder="http://127.0.0.1:7890" onChange={(event) => setUrl(event.target.value)} />
          </Field>
          <Field label={t("provider.proxyUsername")}>
            <Input value={username} onChange={(event) => setUsername(event.target.value)} />
          </Field>
          <Field label={t("provider.proxyPassword")}>
            <Input value={password} onChange={(event) => setPassword(event.target.value)} />
          </Field>
        </>
      ) : null}
    </Modal>
  );
}

/* ============================================================
   Keys page
   ============================================================ */
function QuotaMini({ label, window }: { label: string; window?: OfficialQuotaWindow | null }) {
  const percent = window?.used_percent;
  const width = percent == null || !Number.isFinite(percent) ? 0 : Math.max(0, Math.min(100, percent));
  const reset = quotaResetText(window);
  return (
    <div className="quota-mini">
      <div className="quota-mini-head">
        <span>{label}</span>
        <strong>{formatQuotaPercent(percent)}</strong>
      </div>
      <div className="quota-bar">
        <span style={{ width: `${width}%` }} />
      </div>
      {reset ? <small>{reset}</small> : null}
    </div>
  );
}

function subscriptionDisplay(value: string | null | undefined, providerKind: Provider["kind"]) {
  const label = value?.trim();
  if (!label) return { label: "--", tone: "unknown" };
  const normalized = label.toLowerCase().replace(/[-_\s]+/g, " ");
  const compact = normalized.replace(/\s+/g, "");
  if (
    providerKind === "official_anthropic_cli" ||
    providerKind === "official_anthropic_desktop" ||
    providerKind === "official_anthropic_account"
  ) {
    if (compact === "free") return { label: "FREE", tone: "free" };
    if (compact === "pro") return { label: "PRO", tone: "pro" };
    if (compact === "max" || compact === "max5x" || compact === "max20x") return { label: "MAX", tone: "max" };
    return { label, tone: "unknown" };
  }
  if (compact === "free") return { label: "FREE", tone: "free" };
  if (compact === "plus") return { label: "PLUS", tone: "plus" };
  if (compact === "pro100x" || (compact.startsWith("pro") && compact.includes("100"))) return { label: "PRO 100X", tone: "pro" };
  if (compact === "pro200x" || (compact.startsWith("pro") && compact.includes("200"))) return { label: "PRO 200X", tone: "pro" };
  if (compact === "pro") return { label: "PRO", tone: "pro" };
  return { label, tone: "unknown" };
}

function SubscriptionMini({
  provider,
  quota,
  t,
}: {
  provider: Provider;
  quota?: ProviderUsageStatus["quota"];
  t: ReturnType<typeof useI18n>["t"];
}) {
  const subscription = subscriptionDisplay(quota?.plan_label ?? quota?.plan_type, provider.kind);
  const title = [
    quota?.plan_type ? `plan_type: ${quota.plan_type}` : null,
    quota?.subscription_expires_at ? `expires: ${new Date(quota.subscription_expires_at).toLocaleDateString()}` : null,
  ].filter(Boolean).join(" · ") || undefined;
  return (
    <div className={`usage-mini subscription-mini ${provider.kind.replace(/_/g, "-")} ${subscription.tone}`} title={title}>
      <span>{t("provider.accountPlan")}</span>
      <strong>{subscription.label}</strong>
    </div>
  );
}

function OfficialAccountUsage({
  provider,
  usage,
  onRefresh,
  loading,
  t,
}: {
  provider: Provider;
  usage?: ProviderUsageStatus;
  onRefresh: () => void;
  loading: boolean;
  t: ReturnType<typeof useI18n>["t"];
}) {
  const quota = usage?.quota;
  const local = usage?.local_usage;
  const unknown = local?.unknown_cost_models ?? [];
  const costTitle = unknown.length > 0 ? `Unknown price: ${unknown.join(", ")}` : undefined;
  return (
    <div className="account-usage-strip">
      <SubscriptionMini provider={provider} quota={quota} t={t} />
      <QuotaMini label="5h" window={quota?.five_hour} />
      <QuotaMini label="7d" window={quota?.seven_day} />
      <div className="usage-mini" title={costTitle}>
        <span>{t("provider.usedUsd")}</span>
        <strong>{formatUsd(local?.estimated_cost_usd)}</strong>
      </div>
      <div className="usage-mini">
        <span>{t("provider.totalTokens")}</span>
        <strong>{formatTokens(local?.total_tokens ?? 0)}</strong>
      </div>
      <IconButton
        icon={<RotateCcw size={14} />}
        title={t("provider.refreshUsage")}
        onClick={onRefresh}
        disabled={loading}
      />
      {usage?.error ? <span className="usage-error" title={usage.error}>{usage.error}</span> : null}
    </div>
  );
}

export function KeyVault({ snapshot, config, commit, refresh, notify, notifyRaw, setBusy, busy }: PageProps) {
  const { t } = useI18n();
  const visibleProviders = React.useMemo(
    () => visibleUiProviders(config, snapshot),
    [config.providers, snapshot.keys],
  );
  const [providerModal, setProviderModal] = React.useState<{ open: boolean; editId: string | null }>({ open: false, editId: null });
  const [proxyModal, setProxyModal] = React.useState<Provider | null>(null);
  const [deleteProvider, setDeleteProvider] = React.useState<Provider | null>(null);
  const [usageRefreshing, setUsageRefreshing] = React.useState<string | null>(null);

  async function confirmDeleteProvider() {
    if (!deleteProvider) return;
    const id = deleteProvider.id;
    await commit((d) => {
      d.providers = d.providers.filter((p) => p.id !== id);
      for (const model of d.models) {
        if (model.provider_id === id) {
          model.provider_id = "openai-official";
          model.upstream_model = null;
        }
      }
    }, "toast.providerRemoved");
    setDeleteProvider(null);
  }

  async function refreshUsage(providerId: string) {
    setUsageRefreshing(providerId);
    try {
      await api.refreshProviderUsage(providerId);
    } catch {
      // The backend records the row-level error before returning it.
    } finally {
      await refresh();
      setUsageRefreshing(null);
    }
  }

  function keyStatusMessage(message?: string | null) {
    if (!message) return undefined;
    return message.startsWith("key.") ? t(message as MsgKey) : message;
  }

  function keyStatus(provider: Provider) {
    const status = snapshot.keys.find((k) => k.provider_id === provider.id);
    const present = Boolean(status?.present);
    let tone: "ok" | "warn" | "bad";
    let label: string;
    if (provider.kind === "official_open_ai") {
      if (status?.message === "key.expired" && status.available === false) {
        tone = "bad";
        label = t("key.expired");
      } else {
        tone = present ? "ok" : "warn";
        label = present ? t("key.signedIn") : t("key.notSignedIn");
      }
    } else if (
      provider.kind === "official_anthropic_cli" ||
      provider.kind === "official_anthropic_desktop" ||
      isOfficialAccountProvider(provider)
    ) {
      if (status?.message === "key.expired" && status.available === false) {
        tone = "bad";
        label = t("key.expired");
      } else {
        tone = present ? "ok" : status?.available === false ? "bad" : "warn";
        label = present ? t("key.signedIn") : t("key.notSignedIn");
      }
    } else if (provider.key_ref) {
      tone = present ? "ok" : "warn";
      label = present ? t("key.stored") : t("key.missing");
    } else {
      tone = "ok";
      label = t("key.noKey");
    }
    return { tone, label, message: keyStatusMessage(status?.message), present, available: status?.available !== false };
  }

  function renderRow(provider: Provider, index: number) {
    const ident = providerIcon(provider);
    const editable = provider.kind === "custom" || isOfficialAccountProvider(provider);
    const builtInOfficial = isBuiltInOfficialClient(provider);
    const st = keyStatus(provider);
    const subtitle = provider.kind === "custom" ? t(protocolKey(provider.protocol)) : t(providerShortSourceKey(provider));
    const usage = snapshot.provider_usage.find((item) => item.provider_id === provider.id);
    const isOfficialUsageProvider =
      provider.kind === "official_open_ai" ||
      provider.kind === "official_open_ai_account" ||
      provider.kind === "official_anthropic_cli" ||
      provider.kind === "official_anthropic_desktop" ||
      provider.kind === "official_anthropic_account";
    const showOfficialUsage =
      isOfficialUsageProvider &&
      st.present &&
      st.available;

    return (
      <div className="entity-row provider-row fade-up" key={provider.id} style={{ animationDelay: `${index * 0.04}s` }}>
        <div className="entity-main">
          <span className={`entity-avatar ${ident.cls}`}>{ident.icon}</span>
          <div className="entity-title">
            <div className="provider-title-line">
              <strong>{provider.name}</strong>
              {provider.http_proxy.enabled ? (
                <span className="provider-proxy-icon" title={t("provider.proxyOn")} aria-label={t("provider.proxyOn")}>
                  <RouteProxy size={14} stroke={2.2} />
                </span>
              ) : null}
            </div>
            <span className="entity-sub">{subtitle}</span>
          </div>
        </div>

        <div className="entity-meta">
          {showOfficialUsage ? (
            <OfficialAccountUsage
              provider={provider}
              usage={usage}
              loading={usageRefreshing === provider.id}
              onRefresh={() => refreshUsage(provider.id)}
              t={t}
            />
          ) : (
            <span className="provider-status">
              <Pill tone={st.tone} label={st.label} />
            </span>
          )}
        </div>

        <div className="entity-actions">
          {builtInOfficial ? (
            <IconButton icon={<Settings2 size={15} />} title={t("provider.proxySettings")} onClick={() => setProxyModal(provider)} />
          ) : null}
          {editable ? (
            <>
              <IconButton icon={<Pencil size={15} />} title={t("common.edit")} onClick={() => setProviderModal({ open: true, editId: provider.id })} />
              <IconButton danger icon={<Trash2 size={15} />} title={t("common.delete")} onClick={() => setDeleteProvider(provider)} />
            </>
          ) : null}
        </div>
      </div>
    );
  }

  return (
    <div className="stack page-enter">
      <div className="row row-between wrap">
        <div className="page-lead">{t("keys.subtitle")}</div>
        <Button variant="primary" icon={<Plus size={16} />} onClick={() => setProviderModal({ open: true, editId: null })}>{t("keys.add")}</Button>
      </div>

      <div className="entity-list">
        {visibleProviders.map((p, i) => renderRow(p, i))}
      </div>

      <ProviderModal
        open={providerModal.open}
        onClose={() => setProviderModal({ open: false, editId: null })}
        config={config}
        editId={providerModal.editId}
        commit={commit}
        refresh={refresh}
        setBusy={setBusy}
        busy={busy}
        notify={notify}
        notifyRaw={notifyRaw}
      />
      <ProviderProxyModal
        open={proxyModal !== null}
        onClose={() => setProxyModal(null)}
        provider={proxyModal}
        commit={commit}
        refresh={refresh}
        setBusy={setBusy}
        busy={busy}
        notify={notify}
        notifyRaw={notifyRaw}
      />
      <ConfirmDialog
        open={deleteProvider !== null}
        onClose={() => setDeleteProvider(null)}
        onConfirm={confirmDeleteProvider}
        title={t("provider.deleteTitle")}
        body={deleteProvider ? t("provider.deleteBody", { name: deleteProvider.name }) : ""}
        confirmLabel={t("common.delete")}
        icon={<Trash2 size={18} />}
        loading={busy}
      />
    </div>
  );
}

/* ============================================================
   Logs
   ============================================================ */
const LOG_PAGE_SIZES = [25, 50, 100, 200];

export function Logs({ snapshot, refresh, notify, notifyRaw }: PageProps) {
  const { t } = useI18n();
  const [confirmClear, setConfirmClear] = React.useState(false);
  const [clearing, setClearing] = React.useState(false);
  const [page, setPage] = React.useState(1);
  const [pageSize, setPageSize] = React.useState(50);
  const [records, setRecords] = React.useState<AppSnapshot["requests"]>(snapshot.requests);
  const [total, setTotal] = React.useState(snapshot.request_log_count);
  const [loading, setLoading] = React.useState(false);
  const totalPages = Math.max(1, Math.ceil(total / pageSize));

  const loadPage = React.useCallback(
    async (nextPage = page, nextPageSize = pageSize, silent = false) => {
      if (!silent) setLoading(true);
      try {
        const result = await api.getRequestLogs(nextPage, nextPageSize);
        setRecords(result.records);
        setTotal(result.total);
        setPage(result.page);
        setPageSize(result.page_size);
      } catch (error) {
        notifyRaw(String(error), "bad");
      } finally {
        if (!silent) setLoading(false);
      }
    },
    [page, pageSize, notifyRaw],
  );

  React.useEffect(() => {
    loadPage(page, pageSize);
  }, [loadPage, snapshot.request_log_count]);

  React.useEffect(() => {
    if (!records.some((record) => record.stream_state === "pending")) return;
    const timer = window.setInterval(() => {
      loadPage(page, pageSize, true);
    }, 1000);
    return () => window.clearInterval(timer);
  }, [loadPage, page, pageSize, records]);

  async function clear() {
    setClearing(true);
    try {
      await api.clearRequestLogs();
      notify("toast.logsCleared");
      await refresh();
      await loadPage(1, pageSize);
      setConfirmClear(false);
    } catch (error) {
      notifyRaw(String(error), "bad");
    } finally {
      setClearing(false);
    }
  }

  return (
    <div className="stack page-enter">
      <div className="row row-between wrap">
        <div className="page-lead">{t("logs.summary", { total, page, pages: totalPages })}</div>
        <div className="row wrap">
          <div className="page-size-picker">
            <Dropdown
              value={String(pageSize)}
              options={LOG_PAGE_SIZES.map((size) => ({ value: String(size), label: t("logs.pageSizeValue", { n: size }) }))}
              onChange={(value) => {
                const next = Number(value);
                setPage(1);
                setPageSize(next);
                loadPage(1, next);
              }}
            />
          </div>
          <Button variant="ghost" onClick={() => setPage((p) => Math.max(1, p - 1))} disabled={page <= 1 || loading}>{t("logs.prev")}</Button>
          <Button variant="ghost" onClick={() => setPage((p) => Math.min(totalPages, p + 1))} disabled={page >= totalPages || loading}>{t("logs.next")}</Button>
          <Button variant="ghost" icon={<Trash2 size={16} />} onClick={() => setConfirmClear(true)} disabled={total === 0}>{t("logs.clear")}</Button>
        </div>
      </div>
      <Panel title={t("nav.logs")} sub={t("logs.count", { n: total })} icon={<ListTree size={18} />} color="lav">
        <RequestTable requests={records} models={snapshot.config.models} emptyTitle="logs.empty" emptyHint="logs.emptyHint" />
      </Panel>
      <ConfirmDialog
        open={confirmClear}
        onClose={() => setConfirmClear(false)}
        onConfirm={clear}
        title={t("logs.clearTitle")}
        body={t("logs.clearBody", { n: total })}
        confirmLabel={t("logs.clear")}
        icon={<Trash2 size={18} />}
        loading={clearing}
      />
    </div>
  );
}

/* ============================================================
   Codex Setup wizard
   ============================================================ */
function modelAvailableForCodexMode(
  model: ModelEntry,
  config: AppConfig,
  snapshot: AppSnapshot,
  mode: CodexInjectionMode,
) {
  const provider = config.providers.find((p) => p.id === model.provider_id);
  if (!provider || !model.enabled) return false;
  if (!providerVisibleInUi(provider, snapshot)) return false;
  if (mode === "official_account") return true;
  if (provider.kind === "official_open_ai") return false;

  const key = snapshot.keys.find((item) => item.provider_id === provider.id);
  if (provider.kind === "custom") {
    return provider.key_ref ? Boolean(key?.present && key.available) : true;
  }
  return Boolean(key?.present && key.available);
}

function validCodexSettingModel(current: string | null | undefined, selectable: ModelEntry[]) {
  const selected = current?.trim() ?? "";
  if (selected && selectable.some((model) => model.id === selected)) return selected;
  return selectable[0]?.id ?? "";
}

function validCodexOption(current: string | null | undefined, options: Option[]) {
  const selected = current?.trim() ?? "";
  if (selected && options.some((option) => option.value === selected)) return selected;
  return options[0]?.value ?? "";
}

function lanModelOption(model: LanModelInfo): Option {
  return {
    value: model.id,
    label: model.display_name || model.id,
    sub: `${model.id} · ${formatContext(model.context_window)}`,
  };
}

export function CodexWizard({ snapshot, config, commit, refresh, notify, notifyRaw, setBusy, busy }: PageProps) {
  const { t } = useI18n();
  const mode = config.settings.codex_injection_mode ?? "official_account";
  const lanMode = mode === "lan_share";
  const selectable = lanMode
    ? []
    : config.models.filter((model) => modelAvailableForCodexMode(model, config, snapshot, mode));
  const autoInject = config.settings.auto_inject;
  const [lanHost, setLanHost] = React.useState(config.settings.lan_remote_host);
  const [lanPort, setLanPort] = React.useState(config.settings.lan_remote_port);
  const [lanRemoteKey, setLanRemoteKey] = React.useState(config.settings.lan_remote_api_key);
  const [lanModels, setLanModels] = React.useState<LanModelInfo[]>([]);
  const [lanModelsLoading, setLanModelsLoading] = React.useState(false);
  const [lanSettingsSaving, setLanSettingsSaving] = React.useState(false);
  const [importOpen, setImportOpen] = React.useState(false);
  const [importing, setImporting] = React.useState(false);
  const [configText, setConfigText] = React.useState("");
  const [configLoading, setConfigLoading] = React.useState(false);
  const [configSaving, setConfigSaving] = React.useState(false);
  const [configEditorOpen, setConfigEditorOpen] = React.useState(false);
  const lastApplyErrorRef = React.useRef<string | null>(null);

  const loadCodexConfig = React.useCallback(async () => {
    setConfigLoading(true);
    try {
      const file = await api.readCodexConfig();
      setConfigText(file.content);
    } catch (error) {
      notifyRaw(String(error), "bad");
    } finally {
      setConfigLoading(false);
    }
  }, [notifyRaw]);

  React.useEffect(() => {
    loadCodexConfig();
  }, [loadCodexConfig]);

  React.useEffect(() => {
    const error = snapshot.codex_apply_error;
    if (error && error !== lastApplyErrorRef.current) {
      notifyRaw(error, "bad");
    }
    lastApplyErrorRef.current = error ?? null;
  }, [notifyRaw, snapshot.codex_apply_error]);

  React.useEffect(() => {
    setLanHost(config.settings.lan_remote_host);
    setLanPort(config.settings.lan_remote_port);
    setLanRemoteKey(config.settings.lan_remote_api_key);
  }, [
    config.settings.lan_remote_api_key,
    config.settings.lan_remote_host,
    config.settings.lan_remote_port,
  ]);

  const lanConfigured = Boolean(
    config.settings.lan_remote_host.trim() &&
      config.settings.lan_remote_port > 0 &&
      config.settings.lan_remote_api_key.trim(),
  );

  const loadLanModels = React.useCallback(async () => {
    if (!lanMode || !lanConfigured) {
      setLanModels([]);
      return;
    }
    setLanModelsLoading(true);
    try {
      const result = await api.listLanModels();
      setLanModels(result.models);
    } catch (error) {
      setLanModels([]);
      notifyRaw(String(error), "bad");
    } finally {
      setLanModelsLoading(false);
    }
  }, [lanConfigured, lanMode, notifyRaw]);

  React.useEffect(() => {
    void loadLanModels();
  }, [loadLanModels]);

  async function setDefaultModel(id: string) {
    await commit((d) => {
      d.settings.codex_default_model = id || null;
    });
  }
  async function setFallback(id: string) {
    if (!id) return;
    await commit((d) => {
      d.settings.fallback_model = id;
    });
  }
  async function setImageGenModel(id: string) {
    await commit((d) => {
      d.settings.image_gen_model = id || null;
    });
  }
  async function setAutoInject(v: boolean) {
    await commit((d) => {
      d.settings.auto_inject = v;
      if (v) d.settings.codex_default_model = defaultModel || null;
    }, v ? "toast.autoInjectOn" : "toast.autoInjectOff");
  }
  async function setMode(next: CodexInjectionMode) {
    const nextSelectable = next === "lan_share"
      ? []
      : config.models.filter((model) => modelAvailableForCodexMode(model, config, snapshot, next));
    const nextDefault = next === "lan_share"
      ? ""
      : validCodexSettingModel(config.settings.codex_default_model, nextSelectable);
    const nextFallback = next === "lan_share"
      ? ""
      : validCodexSettingModel(config.settings.fallback_model, nextSelectable);
    await commit((d) => {
      d.settings.codex_injection_mode = next;
      d.settings.codex_default_model = nextDefault || null;
      d.settings.fallback_model = nextFallback || null;
    });
  }

  async function saveLanSettings() {
    setLanSettingsSaving(true);
    try {
      const ok = await commit((d) => {
        d.settings.lan_remote_host = lanHost.trim();
        d.settings.lan_remote_port = Number(lanPort);
        d.settings.lan_remote_api_key = lanRemoteKey.trim();
      });
      if (ok) {
        notify("toast.saved");
        await refresh();
      }
    } finally {
      setLanSettingsSaving(false);
    }
  }

  async function saveCodexConfig() {
    setConfigSaving(true);
    try {
      await api.saveCodexConfig(configText);
      notify("toast.configSaved");
    } catch (error) {
      notifyRaw(String(error), "bad");
    } finally {
      setConfigSaving(false);
    }
  }

  async function importSessions() {
    setImporting(true);
    try {
      const r = await api.importSessions();
      notifyRaw(
        t("setup.importDone", {
          imported: r.imported,
          already: r.already,
          skipped: r.skipped,
          sqlite_updated: r.sqlite_updated,
        }),
      );
      setImportOpen(false);
    } catch (error) {
      notifyRaw(String(error), "bad");
    } finally {
      setImporting(false);
    }
  }

  async function exportCatalog() {
    try {
      await api.exportCatalog();
      notify("toast.catalogExported");
      await refresh();
    } catch (error) {
      notifyRaw(String(error), "bad");
    }
  }
  async function inject() {
    setBusy(true);
    try {
      await api.installCodexConfig(defaultModel);
      notify("toast.configInjected");
      await refresh();
    } catch (error) {
      notifyRaw(String(error), "bad");
    } finally {
      setBusy(false);
    }
  }
  async function restore() {
    setBusy(true);
    try {
      await api.restoreCodexConfig(false);
      notify("toast.configRestored");
      await refresh();
    } catch (error) {
      notifyRaw(String(error), "bad");
    } finally {
      setBusy(false);
    }
  }

  const modelOptions = lanMode
    ? lanModels.map(lanModelOption)
    : selectable.map((m) => ({ value: m.id, label: m.display_name, sub: m.id }));
  const fallbackOptions = lanMode
    ? lanModels.map(lanModelOption)
    : selectable.map((m) => ({ value: m.id, label: m.display_name || m.id, sub: m.id }));
  // 画图模型不依赖当前应用模式(只用于改 image_generation 工具的 model)，所以列全部已配的图片模型 + 「默认」。
  const imageGenOptions = [
    { value: "", label: t("setup.imageGenDefault"), sub: "" },
    ...config.models
      .filter((m) => m.image_generation)
      .map((m) => ({ value: m.id, label: m.display_name || m.id, sub: m.id })),
  ];
  const imageGenModel =
    config.settings.image_gen_model &&
    config.models.some((m) => m.id === config.settings.image_gen_model && m.image_generation)
      ? config.settings.image_gen_model
      : "";
  const defaultModel = lanMode
    ? validCodexOption(config.settings.codex_default_model, modelOptions)
    : validCodexSettingModel(config.settings.codex_default_model, selectable);
  const fallback = lanMode
    ? validCodexOption(config.settings.fallback_model, fallbackOptions)
    : validCodexSettingModel(config.settings.fallback_model, selectable);
  React.useEffect(() => {
    if (!defaultModel && !fallback) return;
    const shouldFixDefault = Boolean(defaultModel) && config.settings.codex_default_model !== defaultModel;
    const shouldFixFallback = Boolean(fallback) && config.settings.fallback_model !== fallback;
    if (!shouldFixDefault && !shouldFixFallback) return;
    void commit((d) => {
      if (defaultModel) d.settings.codex_default_model = defaultModel;
      if (fallback) d.settings.fallback_model = fallback;
    });
  }, [commit, config.settings.codex_default_model, config.settings.fallback_model, defaultModel, fallback]);

  return (
    <div className="stack page-enter">
      <Panel title={t("nav.setup")} sub={t("setup.sub")} icon={<FileJson size={18} />} color="lav" className="setup-panel">
        <Field label={t("setup.mode")}>
          <div className="segmented setup-mode">
            {(["official_account", "third_party_api", "lan_share"] as CodexInjectionMode[]).map((item) => (
              <button
                key={item}
                className={`seg ${mode === item ? "active" : ""}`}
                onClick={() => setMode(item)}
              >
                {t(
                  item === "official_account"
                    ? "setup.modeOfficial"
                    : item === "third_party_api"
                      ? "setup.modeThirdParty"
                      : "setup.modeLanShare",
                )}
              </button>
            ))}
          </div>
        </Field>
        {lanMode && (
          <div className="lan-share-box">
            <div className="grid grid-2">
              <Field label={t("setup.lanHost")}>
                <Input value={lanHost} onChange={(event) => setLanHost(event.target.value)} placeholder="192.168.1.20" />
              </Field>
              <Field label={t("setup.lanPort")}>
                <Input type="number" value={lanPort} onChange={(event) => setLanPort(Number(event.target.value))} />
              </Field>
            </div>
            <Field label={t("setup.lanApiKey")}>
              <Input value={lanRemoteKey} onChange={(event) => setLanRemoteKey(event.target.value)} />
              <div className="field-hint">{t("setup.lanShareHint")}</div>
            </Field>
            <div className="row wrap">
              <Button variant="primary" icon={<ShieldCheck size={16} />} onClick={saveLanSettings} loading={lanSettingsSaving}>
                {t("setup.saveLanShare")}
              </Button>
              <Button variant="ghost" icon={<RotateCcw size={16} />} onClick={loadLanModels} loading={lanModelsLoading} disabled={!lanConfigured}>
                {t("setup.refreshLanModels")}
              </Button>
            </div>
          </div>
        )}
        <div className="grid grid-2" style={{ marginBottom: 16 }}>
          <Field label={t("setup.defaultModel")}>
            <Dropdown value={defaultModel} options={modelOptions} onChange={setDefaultModel} />
          </Field>
          <Field label={t("settings.fallback")}>
            <Dropdown value={fallback} options={fallbackOptions} onChange={setFallback} />
          </Field>
        </div>
        <Field label={t("setup.imageGenModel")} hint={t("setup.imageGenModelHint")}>
          <Dropdown value={imageGenModel} options={imageGenOptions} onChange={setImageGenModel} />
        </Field>

        <div className="modal-toggle-row" style={{ marginBottom: 16 }}>
          <div>
            <div className="mtr-title">{t("setup.autoInject")}</div>
            <div className="mtr-hint">{t("setup.autoInjectHint")}</div>
          </div>
          <Switch checked={autoInject} onChange={setAutoInject} />
        </div>
        {snapshot.codex_apply_error ? (
          <div className="inline-warning" style={{ marginBottom: 16 }}>
            {snapshot.codex_apply_error}
          </div>
        ) : null}

        <Field label={t("setup.codexHome")}>
          <Input value={snapshot.codex_home} readOnly />
        </Field>
        <div className="row wrap" style={{ marginTop: 16 }}>
          <Button variant="ghost" icon={<Pencil size={16} />} onClick={() => setConfigEditorOpen(true)}>{t("setup.configButton")}</Button>
          <Button variant="ghost" icon={<FileJson size={16} />} onClick={exportCatalog}>{t("setup.export")}</Button>
          <Button variant="ghost" icon={<DownloadCloud size={16} />} onClick={() => setImportOpen(true)}>{t("setup.import")}</Button>
          <Button variant="primary" icon={<Sparkles size={16} />} onClick={inject} loading={busy} disabled={autoInject}>{t("setup.inject")}</Button>
          <Button variant="ghost" icon={<RotateCcw size={16} />} onClick={restore} loading={busy}>{t("setup.restore")}</Button>
        </div>
      </Panel>

      <Modal
        open={configEditorOpen}
        onClose={() => setConfigEditorOpen(false)}
        title={t("setup.configFile")}
        sub={snapshot.codex_home}
        icon={<FileJson size={18} />}
        color="mint"
        width={1120}
        footer={
          <>
            <Button variant="ghost" icon={<RotateCcw size={16} />} onClick={loadCodexConfig} loading={configLoading}>{t("setup.reloadConfig")}</Button>
            <Button variant="primary" icon={<FileJson size={16} />} onClick={saveCodexConfig} loading={configSaving}>{t("common.save")}</Button>
          </>
        }
      >
        <div className="codex-config-editor-shell">
          <textarea
            className="input codex-config-editor codex-config-editor-large"
            spellCheck={false}
            autoFocus
            value={configText}
            onChange={(event) => setConfigText(event.target.value)}
          />
        </div>
      </Modal>

      <ConfirmDialog
        open={importOpen}
        onClose={() => setImportOpen(false)}
        onConfirm={importSessions}
        title={t("setup.importTitle")}
        body={t("setup.importBody")}
        confirmLabel={t("setup.importBtn")}
        icon={<DownloadCloud size={18} />}
        tone="primary"
        loading={importing}
      />
    </div>
  );
}
