import React from "react";
import { createPortal } from "react-dom";
import { getVersion } from "@tauri-apps/api/app";
import { openUrl } from "@tauri-apps/plugin-opener";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type DownloadEvent, type Update } from "@tauri-apps/plugin-updater";
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
  IconFileCode as FileJson,
  IconGauge as Gauge,
  IconGripVertical as GripVertical,
  IconInbox as Inbox,
  IconExternalLink as ExternalLink,
  IconKey as KeyRound,
  IconListTree as ListTree,
  IconPencil as Pencil,
  IconPlugConnected as CustomProviderIcon,
  IconPlayerPlay as Play,
  IconPlus as Plus,
  IconRefresh as RotateCcw,
  IconRocket as Rocket,
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
  AppAction,
  AppSnapshot,
  CodexInjectionMode,
  ModelEntry,
  OpenAiQuotaWindow,
  Provider,
  ProviderUsageStatus,
  ProviderProtocol,
  ReasoningEffort,
  StreamState,
  TestModelResult,
  TokenTotals,
} from "./types";
import {
  formatContext,
  formatTokens,
  isOfficialClaude,
  newClaudeAccountProvider,
  newCustomProvider,
  newOpenAiAccountProvider,
  normalizeBaseUrl,
  protocolKey,
  reasoningDefaultsForProtocol,
} from "./providers";
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
import packageMeta from "../package.json";

export type PageProps = {
  snapshot: AppSnapshot;
  config: AppConfig;
  commit: (updater: (draft: AppConfig) => void, toastKey?: MsgKey) => Promise<boolean>;
  refresh: () => Promise<void>;
  notify: (key: MsgKey, tone?: "ok" | "bad") => void;
  notifyRaw: (msg: string, tone?: "ok" | "bad") => void;
  busy: boolean;
  setBusy: (v: boolean) => void;
  appAction: AppAction | null;
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

function quotaResetText(window?: OpenAiQuotaWindow | null) {
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

function streamStateKey(state: StreamState): MsgKey {
  switch (state) {
    case "pending":
      return "stream.pending";
    case "completed":
      return "stream.completed";
    case "failed":
      return "stream.failed";
    case "cancelled":
      return "stream.cancelled";
    case "interrupted":
      return "stream.interrupted";
    case "incomplete":
      return "stream.incomplete";
    case "client_disconnected":
      return "stream.clientDisconnected";
  }
}

function streamStateTone(state: StreamState) {
  if (state === "completed") return "good";
  if (state === "pending") return "pending";
  return "bad";
}

const CODEX_RESERVED_MODEL_IDS = new Set([
  "gpt-5.4-mini",
  "gpt-5.4",
  "gpt-5.3-codex",
  "gpt-5.2-codex",
  "gpt-5.2",
  "gpt-4.1-mini",
]);

function formatBytes(bytes: number) {
  if (bytes >= 1024 * 1024 * 1024) {
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(bytes >= 10 * 1024 * 1024 * 1024 ? 0 : 1)} GB`;
  }
  if (bytes >= 1024 * 1024) {
    return `${(bytes / (1024 * 1024)).toFixed(bytes >= 10 * 1024 * 1024 ? 0 : 1)} MB`;
  }
  if (bytes >= 1024) {
    return `${(bytes / 1024).toFixed(bytes >= 10 * 1024 ? 0 : 1)} KB`;
  }
  return `${bytes} B`;
}

type UpdateStatus =
  | "idle"
  | "checking"
  | "current"
  | "available"
  | "downloading"
  | "installing"
  | "restarting"
  | "error";

function isTauriRuntime() {
  return typeof (window as any).__TAURI_INTERNALS__ !== "undefined";
}

function updateStatusLabel(t: ReturnType<typeof useI18n>["t"], status: UpdateStatus, progress: { downloaded: number; total: number | null }) {
  switch (status) {
    case "checking":
      return t("about.updateChecking");
    case "current":
      return t("about.updateCurrent");
    case "available":
      return t("about.updateAvailable");
    case "downloading":
      return progress.total
        ? t("about.updateDownloading", { done: formatBytes(progress.downloaded), total: formatBytes(progress.total) })
        : t("about.updateDownloadingUnknown", { done: formatBytes(progress.downloaded) });
    case "installing":
      return t("about.updateInstalling");
    case "restarting":
      return t("about.updateRestarting");
    case "error":
      return t("about.updateFailed");
    case "idle":
    default:
      return t("about.updateIdle");
  }
}

/* ============================================================
   About
   ============================================================ */
export function About({ notify, notifyRaw }: PageProps) {
  const { t } = useI18n();
  const [version, setVersion] = React.useState(packageMeta.version);
  const [status, setStatus] = React.useState<UpdateStatus>("idle");
  const [availableUpdate, setAvailableUpdate] = React.useState<Update | null>(null);
  const [updateError, setUpdateError] = React.useState("");
  const [progress, setProgress] = React.useState<{ downloaded: number; total: number | null }>({ downloaded: 0, total: null });

  React.useEffect(() => {
    if (!isTauriRuntime()) return;
    getVersion().then(setVersion).catch(() => undefined);
  }, []);

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

  async function checkForUpdate() {
    setStatus("checking");
    setUpdateError("");
    setAvailableUpdate(null);
    setProgress({ downloaded: 0, total: null });
    try {
      if (!isTauriRuntime()) {
        throw new Error(t("about.updateAppOnly"));
      }
      const result = await check();
      if (!result) {
        setStatus("current");
        return;
      }
      setAvailableUpdate(result);
      setStatus("available");
    } catch (error) {
      setUpdateError(String(error));
      setStatus("error");
    }
  }

  async function installUpdate() {
    if (!availableUpdate) return;
    setStatus("downloading");
    setUpdateError("");
    setProgress({ downloaded: 0, total: null });
    let downloaded = 0;
    try {
      await availableUpdate.downloadAndInstall((event: DownloadEvent) => {
        if (event.event === "Started") {
          downloaded = 0;
          setProgress({ downloaded, total: event.data.contentLength ?? null });
        } else if (event.event === "Progress") {
          downloaded += event.data.chunkLength;
          setProgress((current) => ({ downloaded, total: current.total }));
        } else if (event.event === "Finished") {
          setStatus("installing");
        }
      });
      setStatus("restarting");
      await relaunch();
    } catch (error) {
      setUpdateError(String(error));
      setStatus("error");
    }
  }

  const updatePercent = progress.total ? Math.max(0, Math.min(100, (progress.downloaded / progress.total) * 100)) : 0;
  const statusText = updateStatusLabel(t, status, progress);
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

  return (
    <div className="about-page page-enter">
      <section className="about-hero">
        <div className="about-app">
          <span className="about-app-icon"><img src={appIcon} alt="" /></span>
          <div>
            <h2>Neko Route</h2>
            <p>{t("about.subtitle")}</p>
            <div className="about-version">
              <span>{t("about.currentVersion")}</span>
              <strong>{version}</strong>
              <Pill tone="ok" label="Stable" />
            </div>
          </div>
        </div>
        <Button
          variant="ghost"
          icon={<RotateCcw size={18} />}
          onClick={checkForUpdate}
          loading={status === "checking"}
          disabled={status === "downloading" || status === "installing" || status === "restarting"}
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
              <h3>{t("about.updateTitle")}</h3>
              <p>{t("about.updateSub")}</p>
            </div>
          </div>
          <div className={`update-state ${status}`}>
            <div>
              <strong>{statusText}</strong>
              {availableUpdate ? (
                <span>
                  {t("about.updateVersion", { version: availableUpdate.version })}
                  {availableUpdate.date ? ` · ${new Date(availableUpdate.date).toLocaleDateString()}` : ""}
                </span>
              ) : (
                <span>{t("about.updateEndpoint")}</span>
              )}
            </div>
            {status === "available" ? (
              <Button variant="primary" icon={<DownloadCloud size={16} />} onClick={installUpdate}>
                {t("about.installUpdate")}
              </Button>
            ) : null}
          </div>
          {status === "downloading" || status === "installing" ? (
            <div className="about-progress">
              <span style={{ width: `${updatePercent}%` }} />
            </div>
          ) : null}
          {availableUpdate?.body ? <pre className="about-release-notes">{availableUpdate.body}</pre> : null}
          {updateError ? <div className="inline-warning">{updateError}</div> : null}
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
  emptyTitle = "table.empty",
  emptyHint = "table.emptyHint",
}: {
  requests: AppSnapshot["requests"];
  emptyTitle?: MsgKey;
  emptyHint?: MsgKey;
}) {
  const { t } = useI18n();
  if (requests.length === 0) {
    return <Empty icon={<Inbox size={26} />} title={t(emptyTitle)} hint={t(emptyHint)} />;
  }
  return (
    <div className="table-scroll">
      <div className="table">
        <div className="thead cols-req">
          <span>{t("table.time")}</span>
          <span>{t("table.model")}</span>
          <span>{t("table.providerProtocol")}</span>
          <span>{t("table.reasoning")}</span>
          <span>{t("table.tokens")}</span>
          <span>{t("table.status")}</span>
          <span>{t("table.stream")}</span>
          <span className="hide-sm">{t("table.latency")}</span>
        </div>
        {requests.map((r) => {
          const u = r.usage;
          return (
            <div className="trow cols-req" key={r.id}>
              <span className="mono">{new Date(r.started_at).toLocaleTimeString()}</span>
              <span className="model-cell">
                <strong>{r.model || "—"}</strong>
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
                {u.total_tokens > 0 ? (
                  <span className="tok-cell">
                    <strong>{formatTokens(u.total_tokens)}</strong>
                    <span className="tok-mini">
                      ↑{formatTokens(u.input_tokens)} ↓{formatTokens(u.output_tokens)}
                      {u.cache_read_tokens + u.cache_write_tokens > 0
                        ? ` ⚡${formatTokens(u.cache_read_tokens + u.cache_write_tokens)}`
                        : ""}
                    </span>
                  </span>
                ) : (
                  <span style={{ color: "var(--faint)" }}>—</span>
                )}
              </span>
              <span>
                <span className={`status-chip ${r.status < 400 ? "good" : "bad"}`}>{r.status}</span>
              </span>
              <span>
                {r.stream_state ? (
                  <span
                    className={`stream-chip ${streamStateTone(r.stream_state)}`}
                    title={[
                      r.stream_state === "pending" && r.stream_bytes > 0
                        ? `${t("tokens.streamBytes")}: ${formatBytes(r.stream_bytes)}`
                        : null,
                      r.last_event,
                      r.stream_error,
                    ].filter(Boolean).join(" · ")}
                  >
                    {r.stream_state === "pending" && r.stream_bytes > 0
                      ? formatBytes(r.stream_bytes)
                      : t(streamStateKey(r.stream_state))}
                  </span>
                ) : (
                  <span style={{ color: "var(--faint)" }}>—</span>
                )}
              </span>
              <span className="hide-sm mono">{r.latency_ms}ms</span>
            </div>
          );
        })}
      </div>
    </div>
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
  busy,
}: {
  open: boolean;
  onClose: () => void;
  config: AppConfig;
  commit: PageProps["commit"];
  busy: boolean;
}) {
  const { t } = useI18n();
  const [host, setHost] = React.useState(config.settings.bind_host);
  const [port, setPort] = React.useState(config.settings.port);
  const [allowLan, setAllowLan] = React.useState(config.settings.allow_lan);

  useSeedOnOpen(open, () => {
    setHost(config.settings.bind_host);
    setPort(config.settings.port);
    setAllowLan(config.settings.allow_lan);
  });

  async function submit() {
    const ok = await commit((d) => {
      d.settings.bind_host = host.trim();
      d.settings.port = Number(port);
      d.settings.allow_lan = allowLan;
    });
    if (ok) onClose();
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
        <Switch checked={allowLan} onChange={setAllowLan} />
      </div>
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
  const enabledModels = config.models.filter((m) => m.enabled).length;
  const enabledProviders = config.providers.filter((p) => p.enabled).length;
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

  return (
    <div className="stack page-enter">
      <div className="grid grid-4">
        <Stat icon={<Coins size={15} />} label={t("dash.statTokens")} value={formatTokens(stats.all_time.total_tokens)} foot={t("dash.statTokensFoot", { requests: stats.all_time.requests })} grad />
        <Stat icon={<Cpu size={15} />} label={t("dash.statModels")} value={enabledModels} foot={t("dash.statModelsFoot", { total: config.models.length })} grad />
        <Stat icon={<Server size={15} />} label={t("dash.statProviders")} value={enabledProviders} foot={t("dash.statProvidersFoot", { total: config.providers.length })} grad />
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
        <RequestTable requests={snapshot.requests.slice(0, 6)} />
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
    id: "",
    display_name: "",
    description: "",
    context_window: 258000,
    enabled: true,
    provider_id: provider?.id ?? "",
    upstream_model: null,
    codex_alias: null,
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
  config,
  editIndex,
  commit,
  notifyRaw,
  busy,
}: {
  open: boolean;
  onClose: () => void;
  config: AppConfig;
  editIndex: number | null;
  commit: PageProps["commit"];
  notifyRaw: PageProps["notifyRaw"];
  busy: boolean;
}) {
  const { t } = useI18n();
  const isEdit = editIndex !== null;
  const [draft, setDraft] = React.useState<ModelEntry>(EMPTY_MODEL(config.providers[0]));
  const [upstreamOptions, setUpstreamOptions] = React.useState<Option[]>([]);
  const [loadingUpstream, setLoadingUpstream] = React.useState(false);
  const [upstreamError, setUpstreamError] = React.useState<string | null>(null);
  const fetchToken = React.useRef(0);

  useSeedOnOpen(open, () => {
    if (isEdit && editIndex !== null) setDraft(structuredClone(config.models[editIndex]));
    else setDraft(EMPTY_MODEL(config.providers[0]));
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
  const draftModelId = modelIdKey(draft.id);
  const usesReservedCodexModelId = CODEX_RESERVED_MODEL_IDS.has(draftModelId);
  const duplicateModels = config.models.filter((model, index) => {
    if (isEdit && editIndex === index) return false;
    return sameModelId(model.id, draftModelId);
  });
  const hasDuplicateModelId = duplicateModels.length > 0;

  function setProvider(providerId: string) {
    const provider = config.providers.find((p) => p.id === providerId);
    patch({
      provider_id: providerId,
      ...modelRuntimeDefaults(provider),
    });
  }

  const valid = draft.id.trim().length > 0 && draft.provider_id.length > 0;

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
      upstream_model: draft.upstream_model?.trim() || null,
      ...runtimeDefaults,
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

  const providerOptions = config.providers.map((p) => ({
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
      <div className="grid grid-2">
        <Field label={t("model.id")}>
          <Input value={draft.id} autoFocus placeholder="gpt-5.5" onChange={(e) => patch({ id: e.target.value })} />
        </Field>
        <Field label={t("model.displayName")}>
          <Input value={draft.display_name} placeholder="GPT-5.5" onChange={(e) => patch({ display_name: e.target.value })} />
        </Field>
      </div>
      {usesReservedCodexModelId ? <div className="reserved-model-warning">{t("model.reservedCodexId")}</div> : null}
      {hasDuplicateModelId ? <div className="inline-warning">{isEdit ? t("model.duplicateEditHint") : t("model.duplicateAddHint")}</div> : null}
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
            // Pick fills upstream + auto-fills the model ID; ID stays editable.
            const next: Partial<ModelEntry> = { upstream_model: o.value };
            if (!draft.id.trim() || draft.id.trim() === (draft.upstream_model ?? "").trim()) {
              next.id = o.value;
              if (!draft.display_name.trim()) next.display_name = o.label;
            }
            patch(next);
          }}
        />
        {upstreamError ? (
          <div className="inline-warning">{t("model.upstreamError", { error: upstreamError })}</div>
        ) : null}
      </Field>
      <Field label={t("model.context")}>
        <Input type="number" value={draft.context_window} onChange={(e) => patch({ context_window: Number(e.target.value) })} />
      </Field>
      <Field label={t("model.description")}>
        <Input value={draft.description} onChange={(e) => patch({ description: e.target.value })} />
      </Field>
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
function TestModal({
  open,
  onClose,
  model,
  state,
}: {
  open: boolean;
  onClose: () => void;
  model: string;
  state: { loading: boolean; result: TestModelResult | null };
}) {
  const { t } = useI18n();
  const r = state.result;
  return (
    <Modal
      open={open}
      onClose={onClose}
      title={t("test.title")}
      sub={model}
      icon={<Play size={18} />}
      color="sky"
      width={460}
      footer={<Button variant="ghost" onClick={onClose}>{t("common.close")}</Button>}
    >
      {state.loading ? (
        <div className="test-loading">
          <span className="test-spinner" />
          <p>{t("test.sending", { model })}</p>
        </div>
      ) : r ? (
        r.ok ? (
          <div className="stack">
            <div className="test-reply">
              <div className="test-reply-label">{t("test.reply")} · {t("test.via", { provider: r.provider_name })}</div>
              <p>{r.reply || t("test.noReply")}</p>
            </div>
            <div className="test-meta">
              <Pill tone="ok" label={`${r.status}`} />
              <span className="mono">{r.latency_ms}ms</span>
            </div>
            {r.usage.total_tokens > 0 ? (
              <div className="test-tokens">
                <TokenChip label={t("tokens.input")} value={r.usage.input_tokens} />
                <TokenChip label={t("tokens.output")} value={r.usage.output_tokens} />
                <TokenChip label={t("tokens.total")} value={r.usage.total_tokens} accent />
              </div>
            ) : null}
          </div>
        ) : (
          <div className="test-fail">
            <div className="test-fail-icon">!</div>
            <strong>{t("test.failed")}</strong>
            <p>{r.error === "needs_codex_auth" ? t("test.needsAuth") : r.error}</p>
          </div>
        )
      ) : null}
    </Modal>
  );
}

function TokenChip({ label, value, accent }: { label: string; value: number; accent?: boolean }) {
  return (
    <div className={`token-chip ${accent ? "accent" : ""}`}>
      <span className="tc-label">{label}</span>
      <strong>{formatTokens(value)}</strong>
    </div>
  );
}

/* ============================================================
   Models page
   ============================================================ */
export function ModelGarden({ snapshot, config, commit, busy, notifyRaw, appAction }: PageProps) {
  const { t } = useI18n();
  const [modalOpen, setModalOpen] = React.useState(false);
  const [editIndex, setEditIndex] = React.useState<number | null>(null);
  const [deleteIndex, setDeleteIndex] = React.useState<number | null>(null);
  const [testModal, setTestModal] = React.useState<{ open: boolean; model: string }>({ open: false, model: "" });
  const [testState, setTestState] = React.useState<{ loading: boolean; result: TestModelResult | null }>({ loading: false, result: null });
  const [dragIndex, setDragIndex] = React.useState<number | null>(null);
  const [dragOverIndex, setDragOverIndex] = React.useState<number | null>(null);
  const [dragPointer, setDragPointer] = React.useState<{ x: number; y: number } | null>(null);
  const [dragOffset, setDragOffset] = React.useState<{ x: number; y: number }>({ x: 35, y: 33 });
  const dragFromRef = React.useRef<number | null>(null);
  const dragOverRef = React.useRef<number | null>(null);
  const duplicateModelStats = React.useMemo(() => {
    const stats = new Map<string, { total: number; enabled: number }>();
    for (const model of config.models) {
      const id = modelIdKey(model.id);
      if (!id) continue;
      const current = stats.get(id) ?? { total: 0, enabled: 0 };
      current.total += 1;
      if (model.enabled) current.enabled += 1;
      stats.set(id, current);
    }
    return stats;
  }, [config.models]);

  function openAdd() {
    setEditIndex(null);
    setModalOpen(true);
  }
  function openEdit(i: number) {
    setEditIndex(i);
    setModalOpen(true);
  }

  React.useEffect(() => {
    if (appAction?.type !== "add-model") return;
    openAdd();
  }, [appAction?.nonce]);

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
    setTestModal({ open: true, model: modelId });
    setTestState({ loading: true, result: null });
    try {
      const result = await api.testModel(modelId, providerId);
      setTestState({ loading: false, result });
    } catch (error) {
      notifyRaw(String(error), "bad");
      setTestState({
        loading: false,
        result: { ok: false, status: 0, latency_ms: 0, reply: "", error: String(error), usage: { input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0, total_tokens: 0 }, provider_name: "" },
      });
    }
  }

  const modelTokens = (id: string) =>
    snapshot.stats.by_model.find((m) => m.model === id)?.total_tokens ?? 0;
  const draggingModel = dragIndex !== null ? config.models[dragIndex] : null;
  const draggingProvider = draggingModel
    ? config.providers.find((p) => p.id === draggingModel.provider_id)
    : null;

  return (
    <div className="stack page-enter">
      <div className="row row-between wrap">
        <div className="page-lead">{t("models.count", { count: config.models.length })}</div>
        <Button variant="primary" icon={<Plus size={16} />} onClick={openAdd}>{t("models.add")}</Button>
      </div>

      {config.models.length === 0 ? (
        <Empty icon={<Cpu size={26} />} title={t("models.empty")} hint={t("models.emptyHint")} />
      ) : (
        <div className="entity-list">
          {config.models.map((model, index) => {
            const prov = config.providers.find((p) => p.id === model.provider_id);
            const ident = prov ? providerIcon(prov) : { icon: <CustomProviderIcon size={20} />, cls: "custom" };
            const tokens = modelTokens(model.id);
            const modelStats = duplicateModelStats.get(modelIdKey(model.id));
            const hasDuplicateId = (modelStats?.total ?? 0) > 1;
            const hasEnabledConflict = model.enabled && (modelStats?.enabled ?? 0) > 1;
            const shiftingDown =
              dragIndex !== null &&
              dragOverIndex !== null &&
              dragOverIndex < dragIndex &&
              index >= dragOverIndex &&
              index < dragIndex;
            const shiftingUp =
              dragIndex !== null &&
              dragOverIndex !== null &&
              dragOverIndex > dragIndex &&
              index > dragIndex &&
              index <= dragOverIndex;
            return (
              <div
                className={[
                  "entity-row model-row fade-up",
                  model.enabled ? "" : "off",
                  dragIndex === index ? "dragging" : "",
                  dragOverIndex === index && dragIndex !== index ? "drag-over" : "",
                  dragOverIndex === index && dragIndex !== null && dragOverIndex < dragIndex ? "insert-before" : "",
                  dragOverIndex === index && dragIndex !== null && dragOverIndex > dragIndex ? "insert-after" : "",
                  shiftingDown ? "drag-shift-down" : "",
                  shiftingUp ? "drag-shift-up" : "",
                ].filter(Boolean).join(" ")}
                key={`${model.provider_id}:${model.id}:${index}`}
                data-model-index={index}
                style={{ animationDelay: `${index * 0.04}s` }}
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
                    if (event.key === "ArrowUp" && index > 0) {
                      event.preventDefault();
                      void reorderModels(index, index - 1);
                    }
                    if (event.key === "ArrowDown" && index < config.models.length - 1) {
                      event.preventDefault();
                      void reorderModels(index, index + 1);
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
                    {hasDuplicateId ? (
                      <span className={`entity-note ${hasEnabledConflict ? "warn" : ""}`}>
                        {hasEnabledConflict ? t("model.duplicateEnabledConflict") : t("model.duplicateIdHint")}
                      </span>
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

      <ModelModal open={modalOpen} onClose={() => setModalOpen(false)} config={config} editIndex={editIndex} commit={commit} notifyRaw={notifyRaw} busy={busy} />
      <TestModal open={testModal.open} onClose={() => setTestModal({ open: false, model: "" })} model={testModal.model} state={testState} />
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
  const [enabled, setEnabled] = React.useState(true);
  const [useKey, setUseKey] = React.useState(true);
  const [secret, setSecret] = React.useState("");
  const [tokenJson, setTokenJson] = React.useState("");

  useSeedOnOpen(open, () => {
    if (existing) {
      setFormKind(providerFormKind(existing));
      setName(existing.name);
      setProtocol(existing.protocol);
      setBaseUrl(existing.base_url);
      setEnabled(existing.enabled);
      setUseKey(Boolean(existing.key_ref));
    } else {
      const seed = newCustomProvider();
      setFormKind("custom");
      setName(seed.name);
      setProtocol(seed.protocol);
      setBaseUrl(seed.base_url);
      setEnabled(true);
      setUseKey(true);
    }
    setSecret("");
    setTokenJson("");
  });

  const officialAccount = formKind !== "custom";
  const valid = name.trim().length > 0 && (officialAccount ? isEdit || tokenJson.trim().length > 0 : baseUrl.trim().length > 0);

  const protoOptions = [
    { value: "open_ai_responses", label: t("proto.responses") },
    { value: "open_ai_chat_completions", label: t("proto.chat") },
    { value: "anthropic_messages", label: t("proto.anthropic") },
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
    } else if (kind === "openai_account") {
      const seed = newOpenAiAccountProvider();
      setName(seed.name);
      setProtocol(seed.protocol);
      setBaseUrl(seed.base_url);
      setUseKey(false);
    } else {
      const seed = newClaudeAccountProvider();
      setName(seed.name);
      setProtocol(seed.protocol);
      setBaseUrl(seed.base_url);
      setUseKey(false);
    }
  }

  async function submit() {
    if (!valid) return;
    if (officialAccount && tokenJson.trim()) {
      try {
        JSON.parse(tokenJson);
      } catch (error) {
        notifyRaw(String(error), "bad");
        return;
      }
    }
    let providerId = editId;
    const cleanUrl = officialAccount ? baseUrl : normalizeBaseUrl(baseUrl);
    const ok = await commit((d) => {
      if (existing) {
        const idx = d.providers.findIndex((p) => p.id === existing.id);
        if (idx >= 0) {
          d.providers[idx].name = name.trim();
          d.providers[idx].enabled = enabled;
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
        d.providers.push({ ...seed, name: name.trim(), enabled });
      } else if (formKind === "claude_account") {
        const seed = newClaudeAccountProvider();
        providerId = seed.id;
        d.providers.push({ ...seed, name: name.trim(), enabled });
      } else {
        const seed = newCustomProvider();
        providerId = seed.id;
        d.providers.push({
          ...seed,
          name: name.trim(),
          protocol,
          base_url: cleanUrl,
          enabled,
          key_ref: useKey ? seed.key_ref : null,
        });
      }
    }, isEdit ? "toast.providerUpdated" : "toast.providerAdded");

    if (!ok) return;

    if (officialAccount && tokenJson.trim() && providerId) {
      setBusy(true);
      try {
        await api.setOfficialProviderToken(providerId, tokenJson.trim());
        notify("toast.keySaved");
        await refresh();
      } catch (error) {
        notifyRaw(String(error), "bad");
      } finally {
        setBusy(false);
      }
    } else if (!officialAccount && useKey && secret.trim() && providerId) {
      setBusy(true);
      try {
        await api.setProviderKey(providerId, secret.trim());
        notify("toast.keySaved");
        await refresh();
      } catch (error) {
        notifyRaw(String(error), "bad");
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
      {officialAccount ? (
        <Field label={t("provider.tokenJson")} hint={isEdit ? t("provider.tokenJsonEditHint") : t("provider.tokenJsonHint")}>
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
            <Input value={baseUrl} placeholder="https://api.example.com/v1" onChange={(e) => setBaseUrl(e.target.value)} />
          </Field>
        </>
      )}
      <div className="modal-toggle-row">
        <div className="mtr-title">{t("provider.enabled")}</div>
        <Switch checked={enabled} onChange={setEnabled} />
      </div>
      {!officialAccount ? (
        <div className="modal-toggle-row">
          <div className="mtr-title">{t("provider.useKey")}</div>
          <Switch checked={useKey} onChange={setUseKey} />
        </div>
      ) : null}
      {!officialAccount && useKey ? (
        <Field label={t("keyModal.field")}>
          <Input
            type="password"
            value={secret}
            placeholder={existing?.key_ref ? "••••••••" : t("keyModal.placeholder")}
            onChange={(e) => setSecret(e.target.value)}
          />
        </Field>
      ) : null}
    </Modal>
  );
}

/* ============================================================
   Key-only modal (for official providers that accept keys — currently custom only,
   but kept generic for managing an existing provider's secret)
   ============================================================ */
function KeyModal({
  open,
  onClose,
  provider,
  refresh,
  setBusy,
  busy,
  notify,
  notifyRaw,
}: {
  open: boolean;
  onClose: () => void;
  provider: Provider | null;
  refresh: PageProps["refresh"];
  setBusy: PageProps["setBusy"];
  busy: boolean;
  notify: PageProps["notify"];
  notifyRaw: PageProps["notifyRaw"];
}) {
  const { t } = useI18n();
  const [secret, setSecret] = React.useState("");
  const [editable, setEditable] = React.useState(false);
  const [deletable, setDeletable] = React.useState(false);
  const [loading, setLoading] = React.useState(false);
  const [loadError, setLoadError] = React.useState<string | null>(null);

  useSeedOnOpen(open, () => {
    setSecret("");
    setEditable(false);
    setDeletable(false);
    setLoadError(null);
  });

  React.useEffect(() => {
    if (!open || !provider) return;
    let cancelled = false;
    setLoading(true);
    setLoadError(null);
    api
      .readProviderCredential(provider.id)
      .then((credential) => {
        if (cancelled) return;
        setSecret(credential.value);
        setEditable(credential.editable);
        setDeletable(credential.deletable);
      })
      .catch((error) => {
        if (cancelled) return;
        setSecret("");
        setEditable(false);
        setDeletable(false);
        setLoadError(String(error));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [open, provider?.id]);

  if (!provider) return null;

  const officialTokenProvider = isOfficialAccountProvider(provider);
  const multiline = officialTokenProvider || provider.kind !== "custom";
  const canSave = editable && !loading;
  const canDelete = editable && deletable && !loading && secret.trim().length > 0;

  async function save() {
    if (!provider) return;
    setBusy(true);
    try {
      if (officialTokenProvider) {
        await api.setOfficialProviderToken(provider.id, secret.trim());
      } else {
        await api.setProviderKey(provider.id, secret.trim());
      }
      notify("toast.keySaved");
      await refresh();
      onClose();
    } catch (error) {
      notifyRaw(String(error), "bad");
    } finally {
      setBusy(false);
    }
  }

  async function remove() {
    if (!provider) return;
    setBusy(true);
    try {
      if (officialTokenProvider) {
        await api.deleteOfficialProviderToken(provider.id);
      } else {
        await api.deleteProviderKey(provider.id);
      }
      notify("toast.keyDeleted");
      await refresh();
      onClose();
    } catch (error) {
      notifyRaw(String(error), "bad");
    } finally {
      setBusy(false);
    }
  }

  async function copyCredential() {
    if (!secret) return;
    try {
      await navigator.clipboard.writeText(secret);
      notify("toast.copied");
    } catch (error) {
      notifyRaw(String(error), "bad");
    }
  }

  return (
    <Modal
      open={open}
      onClose={onClose}
      title={t("keyModal.title")}
      sub={t("keyModal.sub", { name: provider.name })}
      icon={<KeyRound size={18} />}
      color="peach"
      width={640}
      footer={
        <>
          {canDelete ? <Button variant="danger" icon={<Trash2 size={16} />} onClick={remove} loading={busy}>{t("keyModal.delete")}</Button> : null}
          <div style={{ flex: 1 }} />
          <Button variant="ghost" onClick={onClose}>{t("common.cancel")}</Button>
          {secret ? <Button variant="ghost" icon={<Copy size={16} />} onClick={copyCredential}>{t("keyModal.copy")}</Button> : null}
          {canSave ? <Button variant="primary" icon={<ShieldCheck size={16} />} onClick={save} loading={busy} disabled={!secret.trim()}>{t("keyModal.save")}</Button> : null}
        </>
      }
    >
      {loadError ? <div className="inline-warning">{t("keyModal.loadError", { error: loadError })}</div> : null}
      <Field
        label={officialTokenProvider ? t("provider.tokenJson") : t("keyModal.field")}
      >
        {multiline ? (
          <textarea
            className="input token-json-input"
            value={loading ? t("keyModal.loading") : secret}
            readOnly={!editable || loading}
            placeholder={t("keyModal.placeholder")}
            onChange={(e) => setSecret(e.target.value)}
            autoFocus
          />
        ) : (
          <Input
            type="text"
            value={loading ? t("keyModal.loading") : secret}
            readOnly={!editable || loading}
            placeholder={t("keyModal.placeholder")}
            onChange={(e) => setSecret(e.target.value)}
            autoFocus
          />
        )}
      </Field>
    </Modal>
  );
}

/* ============================================================
   Keys page
   ============================================================ */
function QuotaMini({ label, window }: { label: string; window?: OpenAiQuotaWindow | null }) {
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

function OpenAiAccountUsage({
  usage,
  onRefresh,
  loading,
  t,
}: {
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

export function KeyVault({ snapshot, config, commit, refresh, notify, notifyRaw, setBusy, busy, appAction }: PageProps) {
  const { t } = useI18n();
  const [providerModal, setProviderModal] = React.useState<{ open: boolean; editId: string | null }>({ open: false, editId: null });
  const [keyModal, setKeyModal] = React.useState<Provider | null>(null);
  const [deleteProvider, setDeleteProvider] = React.useState<Provider | null>(null);
  const [usageRefreshing, setUsageRefreshing] = React.useState<string | null>(null);

  React.useEffect(() => {
    if (appAction?.type !== "add-provider") return;
    setProviderModal({ open: true, editId: null });
  }, [appAction?.nonce]);

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

  async function quickToggle(id: string, v: boolean) {
    await commit((d) => {
      const p = d.providers.find((x) => x.id === id);
      if (p) p.enabled = v;
    });
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
      tone = "warn";
      label = t("key.notSignedIn");
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
    const st = keyStatus(provider);
    const subtitle = provider.kind === "custom" ? t(protocolKey(provider.protocol)) : t(providerShortSourceKey(provider));
    const usage = snapshot.provider_usage.find((item) => item.provider_id === provider.id);
    const isOpenAiUsageProvider = provider.kind === "official_open_ai" || provider.kind === "official_open_ai_account";
    const hasUsageData = Boolean(
      usage?.quota ||
      usage?.local_usage?.total_tokens ||
      usage?.updated_at ||
      usage?.error
    );
    const showOpenAiUsage =
      isOpenAiUsageProvider &&
      (provider.kind === "official_open_ai" || hasUsageData || (st.present && st.available));

    return (
      <div className={`entity-row provider-row fade-up ${provider.enabled ? "" : "off"}`} key={provider.id} style={{ animationDelay: `${index * 0.04}s` }}>
        <div className="entity-main">
          <span className={`entity-avatar ${ident.cls}`}>{ident.icon}</span>
          <div className="entity-title">
            <strong>{provider.name}</strong>
            <span className="entity-sub">{subtitle}</span>
          </div>
        </div>

        <div className="entity-meta">
          {showOpenAiUsage ? (
            <OpenAiAccountUsage
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
          {provider.kind === "custom" && provider.key_ref ? (
            <IconButton icon={<KeyRound size={15} />} title={t("provider.manageKey")} onClick={() => setKeyModal(provider)} />
          ) : null}
          {provider.kind !== "custom" ? (
            <IconButton icon={<KeyRound size={15} />} title={t("provider.manageKey")} onClick={() => setKeyModal(provider)} />
          ) : null}
          {editable ? (
            <>
              <IconButton icon={<Pencil size={15} />} title={t("common.edit")} onClick={() => setProviderModal({ open: true, editId: provider.id })} />
              <IconButton danger icon={<Trash2 size={15} />} title={t("common.delete")} onClick={() => setDeleteProvider(provider)} />
            </>
          ) : null}
          <Switch checked={provider.enabled} onChange={(v) => quickToggle(provider.id, v)} />
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
        {config.providers.map((p, i) => renderRow(p, i))}
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
      <KeyModal
        open={keyModal !== null}
        onClose={() => setKeyModal(null)}
        provider={keyModal}
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
        <RequestTable requests={records} emptyTitle="logs.empty" emptyHint="logs.emptyHint" />
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
  if (mode === "official_account") return model.enabled;

  const provider = config.providers.find((p) => p.id === model.provider_id);
  if (!provider || !provider.enabled || !model.enabled) return false;
  if (provider.kind === "official_open_ai") return false;

  const key = snapshot.keys.find((item) => item.provider_id === provider.id);
  if (provider.kind === "custom") {
    return provider.key_ref ? Boolean(key?.present && key.available) : true;
  }
  return Boolean(key?.present && key.available);
}

export function CodexWizard({ snapshot, config, commit, refresh, notify, notifyRaw, setBusy, busy }: PageProps) {
  const { t } = useI18n();
  const enabled = config.models.filter((m) => m.enabled);
  const mode = config.settings.codex_injection_mode ?? "official_account";
  const selectable = enabled.filter((model) => modelAvailableForCodexMode(model, config, snapshot, mode));
  const defaultModel = config.settings.codex_default_model ?? selectable[0]?.id ?? enabled[0]?.id ?? "";
  const autoInject = config.settings.auto_inject;
  const [importOpen, setImportOpen] = React.useState(false);
  const [importing, setImporting] = React.useState(false);
  const [configText, setConfigText] = React.useState("");
  const [configLoading, setConfigLoading] = React.useState(false);
  const [configSaving, setConfigSaving] = React.useState(false);
  const [configEditorOpen, setConfigEditorOpen] = React.useState(false);

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
  async function setAutoInject(v: boolean) {
    await commit((d) => {
      d.settings.auto_inject = v;
      // Lock in the current default so backend auto-injection knows what to write.
      if (v && !d.settings.codex_default_model) {
        d.settings.codex_default_model = defaultModel || null;
      }
    }, v ? "toast.autoInjectOn" : "toast.autoInjectOff");
  }
  async function setMode(next: CodexInjectionMode) {
    const nextSelectable = config.models.filter((model) => modelAvailableForCodexMode(model, config, snapshot, next));
    const nextFallback = nextSelectable.some((model) => model.id === config.settings.fallback_model)
      ? config.settings.fallback_model
      : nextSelectable[0]?.id ?? null;
    await commit((d) => {
      d.settings.codex_injection_mode = next;
      d.settings.fallback_model = nextFallback;
    });
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

  const modelOptions = selectable.map((m) => ({ value: m.id, label: m.display_name, sub: m.id }));
  const fallbackOptions = selectable.map((m) => ({ value: m.id, label: m.display_name || m.id, sub: m.id }));
  const fallback = fallbackOptions.some((option) => option.value === config.settings.fallback_model)
    ? config.settings.fallback_model ?? ""
    : fallbackOptions[0]?.value ?? "";
  React.useEffect(() => {
    if (!fallback || config.settings.fallback_model === fallback) return;
    void commit((d) => {
      d.settings.fallback_model = fallback;
    });
  }, [commit, config.settings.fallback_model, fallback]);

  return (
    <div className="stack page-enter">
      <Panel title={t("nav.setup")} sub={t("setup.sub")} icon={<FileJson size={18} />} color="lav">
        <Field label={t("setup.mode")}>
          <div className="segmented setup-mode">
            {(["official_account", "third_party_api"] as CodexInjectionMode[]).map((item) => (
              <button
                key={item}
                className={`seg ${mode === item ? "active" : ""}`}
                onClick={() => setMode(item)}
              >
                {t(item === "official_account" ? "setup.modeOfficial" : "setup.modeThirdParty")}
              </button>
            ))}
          </div>
        </Field>
        <div className="grid grid-2" style={{ marginBottom: 16 }}>
          <Field label={t("setup.defaultModel")}>
            <Dropdown value={defaultModel} options={modelOptions} onChange={setDefaultModel} />
          </Field>
          <Field label={t("settings.fallback")}>
            <Dropdown value={fallback} options={fallbackOptions} onChange={setFallback} />
          </Field>
        </div>

        <div className="modal-toggle-row" style={{ marginBottom: 16 }}>
          <div>
            <div className="mtr-title">{t("setup.autoInject")}</div>
            <div className="mtr-hint">{t("setup.autoInjectHint")}</div>
          </div>
          <Switch checked={autoInject} onChange={setAutoInject} />
        </div>

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
