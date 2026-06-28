import React from "react";
import { createPortal } from "react-dom";
import ReactDOM from "react-dom/client";
import { getVersion } from "@tauri-apps/api/app";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type DownloadEvent, type Update } from "@tauri-apps/plugin-updater";
import {
  IconActivity as Activity,
  IconActivityHeartbeat as HeartPulse,
  IconAlertTriangle as TriangleAlert,
  IconCheck as Check,
  IconCloudDownload as DownloadCloud,
  IconDatabase as Database,
  IconInfoCircle as InfoCircle,
  IconKey as KeyRound,
  IconListTree as ListTree,
  IconMinus as Minus,
  IconRefresh as RotateCcw,
  IconSettings as Settings2,
  IconWorldShare as Share2,
  IconSparkles as Sparkles,
  IconSquare as Square,
  IconX as X,
} from "@tabler/icons-react";
import { api } from "./api";
import type { AppConfig, AppSnapshot, Page } from "./types";
import type { MsgKey } from "./messages";
import { I18nProvider, useI18n } from "./i18n";
import { ThemeProvider } from "./theme";
import { Petals } from "./Petals";
import { ConfirmDialog, IconButton, LangSwitch, ThemeSwitch } from "./ui";
import { Button } from "./ui";
import {
  CodexWizard,
  About,
  Dashboard,
  HealthPage,
  KeyVault,
  Logs,
  ModelGarden,
  PageProps,
  SettingsModal,
  SharePage,
  visibleUiModelCount,
  visibleUiProviders,
} from "./pages";
import {
  fetchLatestReleaseNotes,
  fetchReleaseNotesForVersion,
  isTauriRuntime,
  releaseDateText,
  releaseNotesFromUpdate,
  updateStatusLabel,
  type ReleaseNotes,
  type UpdateProgress,
  type UpdateStatus,
} from "./updates";
import appIcon from "./assets/app-icon.png";
import packageMeta from "../package.json";
import "./styles.css";

type NavItem = { page: Page; label: MsgKey; icon: React.ComponentType<{ size?: number }> };

const NAV: NavItem[] = [
  { page: "dashboard", label: "nav.dashboard", icon: Activity },
  { page: "models", label: "nav.models", icon: Database },
  { page: "keys", label: "nav.keys", icon: KeyRound },
  { page: "logs", label: "nav.logs", icon: ListTree },
  { page: "health", label: "nav.health", icon: HeartPulse },
  { page: "share", label: "nav.share", icon: Share2 },
  { page: "wizard", label: "nav.setup", icon: Sparkles },
  { page: "about", label: "nav.about", icon: InfoCircle },
];

function navForMode(mode: AppConfig["settings"]["codex_injection_mode"]) {
  if (mode !== "lan_share") return NAV;
  return NAV.filter((item) => item.page !== "models" && item.page !== "keys");
}

type Toast = { id: number; msg: string; tone: "ok" | "bad" };

function AppTitlebar() {
  const isMac = typeof navigator !== "undefined" && /mac/i.test(navigator.platform);
  const appWindow = React.useMemo(() => (isTauriRuntime() ? getCurrentWindow() : null), []);
  const [maximized, setMaximized] = React.useState(false);

  React.useEffect(() => {
    document.documentElement.dataset.nativeTitlebar = "false";
  }, []);

  const refreshMaximized = React.useCallback(() => {
    appWindow?.isMaximized().then(setMaximized).catch(() => undefined);
  }, [appWindow]);

  React.useEffect(() => {
    refreshMaximized();
  }, [refreshMaximized]);

  const startDrag = React.useCallback((event: React.MouseEvent<HTMLDivElement>) => {
    if (!appWindow || event.button !== 0 || event.detail > 1) return;
    event.preventDefault();
    appWindow.startDragging().catch(() => undefined);
  }, [appWindow]);

  const toggleMaximize = React.useCallback(() => {
    appWindow?.toggleMaximize().then(refreshMaximized).catch(() => undefined);
  }, [appWindow, refreshMaximized]);

  const controls = (
    <div className="window-controls" onMouseDown={(event) => event.stopPropagation()}>
      <button className="window-control minimize" title="Minimize" onClick={() => appWindow?.minimize().catch(() => undefined)}>
        {isMac ? null : <Minus size={15} />}
      </button>
      <button className="window-control maximize" title={maximized ? "Restore" : "Maximize"} onClick={toggleMaximize}>
        {isMac ? null : <Square size={13} />}
      </button>
      <button className="window-control close" title="Close" onClick={() => appWindow?.close().catch(() => undefined)}>
        {isMac ? null : <X size={16} />}
      </button>
    </div>
  );

  return (
    <div className={`app-titlebar ${isMac ? "mac" : "standard"}`}>
      <div className="titlebar-drag" onMouseDown={startDrag} onDoubleClick={toggleMaximize}>
        <img src={appIcon} alt="" />
        <strong>Neko Route</strong>
      </div>
      {isMac ? null : controls}
    </div>
  );
}

function UpdateDialog({
  open,
  onClose,
  onInstall,
  status,
  progress,
  update,
  release,
  releaseLoading,
  error,
}: {
  open: boolean;
  onClose: () => void;
  onInstall: () => void;
  status: UpdateStatus;
  progress: UpdateProgress;
  update: Update | null;
  release: ReleaseNotes | null;
  releaseLoading: boolean;
  error: string;
}) {
  const { t } = useI18n();
  const busy = status === "downloading" || status === "installing" || status === "restarting";
  const canInstall = Boolean(update) && !busy;
  const statusText = updateStatusLabel(t, status, progress);
  const updateDate = releaseDateText(release?.publishedAt ?? update?.date);
  const updatePercent = progress.total ? Math.max(0, Math.min(100, (progress.downloaded / progress.total) * 100)) : 0;

  if (!open) return null;

  return createPortal(
    <div className="update-dialog-overlay" role="presentation">
      <div className="update-dialog" role="dialog" aria-modal="true" aria-labelledby="update-dialog-title">
        <h2 id="update-dialog-title">{t("about.updateDialogTitle")}</h2>
        <div className={`update-dialog-state ${status}`}>
          <div>
            <strong>{statusText}</strong>
            {update?.version ? (
              <span>
                {t("about.updateVersion", { version: update.version })}
                {updateDate ? ` · ${updateDate}` : ""}
              </span>
            ) : null}
          </div>
        </div>
        {busy ? (
          <div className="about-progress update-dialog-progress">
            <span style={{ width: `${updatePercent}%` }} />
          </div>
        ) : null}
        <div className="update-dialog-notes-head">{t("about.updateDialogLatestNotes")}</div>
        {releaseLoading ? (
          <div className="about-release-placeholder">{t("about.releaseLoading")}</div>
        ) : release?.body ? (
          <pre className="update-dialog-notes">{release.body}</pre>
        ) : (
          <div className="about-release-placeholder">{t("about.releaseEmpty")}</div>
        )}
        {release?.error ? <div className="inline-warning">{t("about.releaseLoadFailed", { error: release.error })}</div> : null}
        {error ? <div className="inline-warning">{error}</div> : null}
        <div className="update-dialog-actions">
          <Button variant="ghost" onClick={onClose} disabled={busy}>
            {t("common.cancel")}
          </Button>
          <Button variant="primary" icon={<DownloadCloud size={17} />} onClick={onInstall} disabled={!canInstall} loading={busy}>
            {t("about.updateNow")}
          </Button>
        </div>
      </div>
    </div>,
    document.body,
  );
}

function App() {
  const { t } = useI18n();
  const [snapshot, setSnapshot] = React.useState<AppSnapshot | null>(null);
  const [config, setConfig] = React.useState<AppConfig | null>(null);
  const [page, setPage] = React.useState<Page>("dashboard");
  const [busy, setBusy] = React.useState(false);
  const [toasts, setToasts] = React.useState<Toast[]>([]);
  const [settingsOpen, setSettingsOpen] = React.useState(false);
  const [appVersion, setAppVersion] = React.useState(packageMeta.version);
  const [updateStatus, setUpdateStatus] = React.useState<UpdateStatus>("idle");
  const [availableUpdate, setAvailableUpdate] = React.useState<Update | null>(null);
  const [latestRelease, setLatestRelease] = React.useState<ReleaseNotes | null>(null);
  const [latestReleaseLoading, setLatestReleaseLoading] = React.useState(false);
  const [currentRelease, setCurrentRelease] = React.useState<ReleaseNotes | null>(null);
  const [currentReleaseLoading, setCurrentReleaseLoading] = React.useState(false);
  const [currentReleaseError, setCurrentReleaseError] = React.useState("");
  const [updateError, setUpdateError] = React.useState("");
  const [updateProgress, setUpdateProgress] = React.useState<UpdateProgress>({ downloaded: 0, total: null });
  const [updateDialogOpen, setUpdateDialogOpen] = React.useState(false);
  const [codexRestartConfirmOpen, setCodexRestartConfirmOpen] = React.useState(false);
  const [codexRestarting, setCodexRestarting] = React.useState(false);
  const updateCheckSeq = React.useRef(0);
  const autoUpdateCheckStarted = React.useRef(false);

  const pushToast = React.useCallback((msg: string, tone: "ok" | "bad") => {
    const id = Date.now() + Math.random();
    setToasts((cur) => [...cur, { id, msg, tone }]);
    window.setTimeout(() => setToasts((cur) => cur.filter((x) => x.id !== id)), tone === "ok" ? 1700 : 4200);
  }, []);

  const notify = React.useCallback((key: MsgKey, tone: "ok" | "bad" = "ok") => pushToast(t(key), tone), [pushToast, t]);
  const notifyRaw = React.useCallback((msg: string, tone: "ok" | "bad" = "ok") => pushToast(msg, tone), [pushToast]);

  const refresh = React.useCallback(async () => {
    const next = await api.getSnapshot();
    setSnapshot(next);
    setConfig(next.config);
  }, []);

  React.useEffect(() => {
    refresh().catch((e) => notifyRaw(String(e), "bad"));
    const timer = window.setInterval(() => refresh().catch(() => undefined), 4000);
    return () => window.clearInterval(timer);
  }, [refresh, notifyRaw]);

  React.useEffect(() => {
    if (!isTauriRuntime()) return;
    getVersion().then(setAppVersion).catch(() => undefined);
  }, []);

  React.useEffect(() => {
    let cancelled = false;
    setCurrentReleaseLoading(true);
    setCurrentReleaseError("");
    fetchReleaseNotesForVersion(appVersion)
      .then((release) => {
        if (cancelled) return;
        setCurrentRelease(release);
      })
      .catch((error) => {
        if (cancelled) return;
        setCurrentRelease(null);
        setCurrentReleaseError(String(error));
      })
      .finally(() => {
        if (!cancelled) setCurrentReleaseLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [appVersion]);

  const checkForUpdate = React.useCallback(async () => {
    const seq = updateCheckSeq.current + 1;
    updateCheckSeq.current = seq;
    setUpdateStatus("checking");
    setUpdateError("");
    setAvailableUpdate(null);
    setLatestRelease(null);
    setLatestReleaseLoading(false);
    setUpdateProgress({ downloaded: 0, total: null });
    try {
      if (!isTauriRuntime()) {
        throw new Error(t("about.updateAppOnly"));
      }
      const result = await check();
      if (seq !== updateCheckSeq.current) return;
      if (!result) {
        setUpdateStatus("current");
        setUpdateDialogOpen(false);
        return;
      }
      setAvailableUpdate(result);
      setLatestRelease(releaseNotesFromUpdate(result));
      setUpdateStatus("available");
      setUpdateDialogOpen(true);
      setLatestReleaseLoading(true);
      fetchLatestReleaseNotes()
        .then((release) => {
          if (seq === updateCheckSeq.current) setLatestRelease(release);
        })
        .catch((error) => {
          if (seq === updateCheckSeq.current) setLatestRelease(releaseNotesFromUpdate(result, String(error)));
        })
        .finally(() => {
          if (seq === updateCheckSeq.current) setLatestReleaseLoading(false);
        });
    } catch (error) {
      if (seq !== updateCheckSeq.current) return;
      setUpdateError(String(error));
      setUpdateStatus("error");
    }
  }, [t]);

  const installUpdate = React.useCallback(async () => {
    if (!availableUpdate) return;
    setUpdateStatus("downloading");
    setUpdateError("");
    setUpdateDialogOpen(true);
    setUpdateProgress({ downloaded: 0, total: null });
    let downloaded = 0;
    try {
      await availableUpdate.downloadAndInstall((event: DownloadEvent) => {
        if (event.event === "Started") {
          downloaded = 0;
          setUpdateProgress({ downloaded, total: event.data.contentLength ?? null });
        } else if (event.event === "Progress") {
          downloaded += event.data.chunkLength;
          setUpdateProgress((current) => ({ downloaded, total: current.total }));
        } else if (event.event === "Finished") {
          setUpdateStatus("installing");
        }
      });
      setUpdateStatus("restarting");
      await relaunch();
    } catch (error) {
      setUpdateError(String(error));
      setUpdateStatus("error");
    }
  }, [availableUpdate]);

  const performCodexRestart = React.useCallback(async () => {
    setCodexRestarting(true);
    try {
      const result = await api.restartCodexApp();
      notify(result.action === "restarted" ? "codexRestart.restarted" : "codexRestart.started");
    } catch (error) {
      notifyRaw(String(error), "bad");
    } finally {
      setCodexRestarting(false);
    }
  }, [notify, notifyRaw]);

  const triggerCodexRestart = React.useCallback(async () => {
    // Windows 下检测不到 Codex 进程时直接提示手动启动(无法可靠自动拉起)；其他平台照常重启。
    if (/win/i.test(navigator.platform)) {
      try {
        const status = await api.codexAppStatus();
        if (!status.running) {
          notify("codexRestart.notRunning");
          return;
        }
      } catch {
        // 状态检测失败就照常走确认流程
      }
    }
    setCodexRestartConfirmOpen(true);
  }, [notify]);

  const confirmCodexRestart = React.useCallback(async () => {
    setCodexRestartConfirmOpen(false);
    await performCodexRestart();
  }, [performCodexRestart]);

  React.useEffect(() => {
    if (!isTauriRuntime()) return;
    if (autoUpdateCheckStarted.current) return;
    autoUpdateCheckStarted.current = true;
    checkForUpdate().catch(() => undefined);
  }, [checkForUpdate]);

  React.useEffect(() => {
    if (page !== "dashboard" || !snapshot?.requests.some((request) => request.stream_state === "pending")) {
      return;
    }
    const timer = window.setInterval(() => refresh().catch(() => undefined), 1000);
    return () => window.clearInterval(timer);
  }, [page, refresh, snapshot?.requests]);

  React.useEffect(() => {
    if (config?.settings.codex_injection_mode === "lan_share" && (page === "models" || page === "keys")) {
      setPage("wizard");
    }
  }, [config?.settings.codex_injection_mode, page]);

  const commit = React.useCallback<PageProps["commit"]>(
    async (updater, toastKey) => {
      if (!config) return false;
      const draft = structuredClone(config) as AppConfig;
      updater(draft);
      setBusy(true);
      try {
        const next = await api.saveConfig(draft);
        setSnapshot(next);
        setConfig(next.config);
        notify(toastKey ?? "toast.saved");
        return true;
      } catch (error) {
        notifyRaw(String(error), "bad");
        return false;
      } finally {
        setBusy(false);
      }
    },
    [config, notify, notifyRaw],
  );

  if (!snapshot || !config) {
    return (
      <>
        <Petals />
        <AppTitlebar />
        <div className="loading">
          <span className="l-mark"><img src={appIcon} alt="Neko Route" /></span>
          <p>{t("loading.text")}</p>
          <span className="l-bar" />
        </div>
      </>
    );
  }

  const pageProps: PageProps = {
    snapshot,
    config,
    commit,
    refresh,
    notify,
    notifyRaw,
    busy,
    setBusy,
    appVersion,
    updateStatus,
    availableUpdateVersion: availableUpdate?.version ?? null,
    currentRelease,
    currentReleaseLoading,
    currentReleaseError,
    checkForUpdate,
  };
  const navItems = navForMode(config.settings.codex_injection_mode);
  const active = navItems.find((n) => n.page === page) ?? NAV.find((n) => n.page === page) ?? navItems[0];
  const badges: Partial<Record<Page, number>> = {
    models: visibleUiModelCount(config, snapshot),
    keys: visibleUiProviders(config, snapshot).length,
    logs: snapshot.request_log_count,
  };

  return (
    <>
      <Petals />
      <AppTitlebar />
      <div className="app-shell">
        <aside className="sidebar">
          <div className="brand">
            <span className="brand-mark"><img src={appIcon} alt="" /></span>
            <div className="brand-text">
              <strong>Neko Route</strong>
              <span>{t("app.tagline")}</span>
            </div>
          </div>

          <nav className="nav">
            {navItems.map((item) => {
              const Icon = item.icon;
              const badge = badges[item.page];
              return (
                <button key={item.page} className={`nav-item ${page === item.page ? "active" : ""}`} onClick={() => setPage(item.page)}>
                  <Icon size={18} />
                  <span>{t(item.label)}</span>
                  {badge ? <span className="nav-badge">{badge}</span> : null}
                </button>
              );
            })}
          </nav>

          <div className="side-status">
            <span className={`ss-dot ${snapshot.server.running ? "ok" : "bad"}`} />
            <span className="ss-text">{snapshot.server.running ? t("common.running") : t("common.stopped")}</span>
          </div>
        </aside>

        <main className="workspace">
          <header className="topbar">
            <div className="topbar-title">
              <h1>{t(active.label)}</h1>
              <div className="sub">
                <span>{t("topbar.endpoint")}</span>
                <code>{snapshot.server.bind_url}</code>
              </div>
            </div>
            <div className="top-actions">
              <IconButton icon={<Settings2 size={18} />} title={t("dash.editSettings")} onClick={() => setSettingsOpen(true)} />
              <IconButton icon={<RotateCcw size={18} />} title={t("codexRestart.button")} onClick={triggerCodexRestart} disabled={codexRestarting} />
              <ThemeSwitch />
              <LangSwitch />
            </div>
          </header>

          <div className="scroll-area">
            {page === "dashboard" && <Dashboard {...pageProps} />}
            {page === "models" && <ModelGarden {...pageProps} />}
            {page === "keys" && <KeyVault {...pageProps} />}
            {page === "logs" && <Logs {...pageProps} />}
            {page === "health" && <HealthPage {...pageProps} />}
            {page === "share" && <SharePage {...pageProps} />}
            {page === "wizard" && <CodexWizard {...pageProps} />}
            {page === "about" && <About {...pageProps} />}
          </div>
        </main>
      </div>

      <div className="toast-wrap">
        {toasts.map((x) => (
          <div className={`toast ${x.tone}`} key={x.id}>
            <span className="t-icon">{x.tone === "ok" ? <Check size={16} /> : <TriangleAlert size={16} />}</span>
            {x.msg}
          </div>
        ))}
      </div>

      <SettingsModal
        open={settingsOpen}
        onClose={() => setSettingsOpen(false)}
        config={config}
        commit={commit}
        refresh={refresh}
        notify={notify}
        notifyRaw={notifyRaw}
        busy={busy}
      />
      <UpdateDialog
        open={updateDialogOpen}
        onClose={() => setUpdateDialogOpen(false)}
        onInstall={installUpdate}
        status={updateStatus}
        progress={updateProgress}
        update={availableUpdate}
        release={latestRelease}
        releaseLoading={latestReleaseLoading}
        error={updateError}
      />
      <ConfirmDialog
        open={codexRestartConfirmOpen}
        onClose={() => setCodexRestartConfirmOpen(false)}
        onConfirm={confirmCodexRestart}
        title={t("codexRestart.confirmTitle")}
        body={t("codexRestart.confirmBody")}
        confirmLabel={t("codexRestart.confirm")}
        icon={<RotateCcw size={18} />}
        tone="primary"
        loading={codexRestarting}
      />
    </>
  );
}

// Reuse a single root across HMR / re-execution so dev hot-reloads don't
// warn about calling createRoot twice on the same container.
const container = document.getElementById("root")!;
type RootHost = typeof globalThis & { __nekoRoot?: ReactDOM.Root };
const host = globalThis as RootHost;
const root = host.__nekoRoot ?? (host.__nekoRoot = ReactDOM.createRoot(container));

root.render(
  <React.StrictMode>
    <ThemeProvider>
      <I18nProvider>
        <App />
      </I18nProvider>
    </ThemeProvider>
  </React.StrictMode>,
);
