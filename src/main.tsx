import React from "react";
import ReactDOM from "react-dom/client";
import { listen } from "@tauri-apps/api/event";
import {
  IconActivity as Activity,
  IconAlertTriangle as TriangleAlert,
  IconCheck as Check,
  IconDatabase as Database,
  IconInfoCircle as InfoCircle,
  IconKey as KeyRound,
  IconListTree as ListTree,
  IconRefresh as RotateCcw,
  IconSettings as Settings2,
  IconSparkles as Sparkles,
} from "@tabler/icons-react";
import { api } from "./api";
import type { AppAction, AppConfig, AppSnapshot, Page } from "./types";
import type { MsgKey } from "./messages";
import { I18nProvider, useI18n } from "./i18n";
import { ThemeProvider } from "./theme";
import { Petals } from "./Petals";
import { IconButton, LangSwitch, ThemeSwitch, Pill } from "./ui";
import {
  CodexWizard,
  About,
  Dashboard,
  KeyVault,
  Logs,
  ModelGarden,
  PageProps,
  SettingsModal,
} from "./pages";
import appIcon from "./assets/app-icon.png";
import "./styles.css";

type NavItem = { page: Page; label: MsgKey; icon: React.ComponentType<{ size?: number }> };

const NAV: NavItem[] = [
  { page: "dashboard", label: "nav.dashboard", icon: Activity },
  { page: "models", label: "nav.models", icon: Database },
  { page: "keys", label: "nav.keys", icon: KeyRound },
  { page: "logs", label: "nav.logs", icon: ListTree },
  { page: "wizard", label: "nav.setup", icon: Sparkles },
  { page: "about", label: "nav.about", icon: InfoCircle },
];

type Toast = { id: number; msg: string; tone: "ok" | "bad" };

function App() {
  const { t } = useI18n();
  const [snapshot, setSnapshot] = React.useState<AppSnapshot | null>(null);
  const [config, setConfig] = React.useState<AppConfig | null>(null);
  const [page, setPage] = React.useState<Page>("dashboard");
  const [busy, setBusy] = React.useState(false);
  const [toasts, setToasts] = React.useState<Toast[]>([]);
  const [appAction, setAppAction] = React.useState<AppAction | null>(null);
  const [settingsOpen, setSettingsOpen] = React.useState(false);

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
    if (page !== "dashboard" || !snapshot?.requests.some((request) => request.stream_state === "pending")) {
      return;
    }
    const timer = window.setInterval(() => refresh().catch(() => undefined), 1000);
    return () => window.clearInterval(timer);
  }, [page, refresh, snapshot?.requests]);

  React.useEffect(() => {
    if (typeof (window as any).__TAURI_INTERNALS__ === "undefined") return;
    let unlistenProvider: (() => void) | undefined;
    let unlistenModel: (() => void) | undefined;
    listen("neko-route://add-provider", () => {
      setPage("keys");
      setAppAction({ type: "add-provider", nonce: Date.now() + Math.random() });
    }).then((dispose) => {
      unlistenProvider = dispose;
    }).catch(() => undefined);
    listen("neko-route://add-model", () => {
      setPage("models");
      setAppAction({ type: "add-model", nonce: Date.now() + Math.random() });
    }).then((dispose) => {
      unlistenModel = dispose;
    }).catch(() => undefined);
    return () => {
      unlistenProvider?.();
      unlistenModel?.();
    };
  }, []);

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
        <div className="loading">
          <span className="l-mark"><img src={appIcon} alt="Neko Route" /></span>
          <p>{t("loading.text")}</p>
          <span className="l-bar" />
        </div>
      </>
    );
  }

  const pageProps: PageProps = { snapshot, config, commit, refresh, notify, notifyRaw, busy, setBusy, appAction };
  const active = NAV.find((n) => n.page === page)!;
  const badges: Partial<Record<Page, number>> = {
    models: config.models.length,
    keys: config.providers.length,
    logs: snapshot.request_log_count,
  };

  return (
    <>
      <Petals />
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
            {NAV.map((item) => {
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
              <Pill tone={snapshot.server.running ? "ok" : "bad"} label={snapshot.server.running ? t("topbar.serverOnline") : t("topbar.serverIssue")} />
              <IconButton icon={<Settings2 size={18} />} title={t("dash.editSettings")} onClick={() => setSettingsOpen(true)} />
              <IconButton icon={<RotateCcw size={18} />} title={t("common.refresh")} onClick={() => refresh()} />
              <ThemeSwitch />
              <LangSwitch />
            </div>
          </header>

          <div className="scroll-area">
            {page === "dashboard" && <Dashboard {...pageProps} />}
            {page === "models" && <ModelGarden {...pageProps} />}
            {page === "keys" && <KeyVault {...pageProps} />}
            {page === "logs" && <Logs {...pageProps} />}
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
        busy={busy}
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
