import type { Update } from "@tauri-apps/plugin-updater";
import type { MsgKey, Vars } from "./messages";

export type UpdateStatus =
  | "idle"
  | "checking"
  | "current"
  | "available"
  | "downloading"
  | "installing"
  | "restarting"
  | "error";

export type UpdateProgress = {
  downloaded: number;
  total: number | null;
};

export type ReleaseNotes = {
  version: string;
  tagName: string;
  name: string;
  body: string;
  publishedAt?: string;
  url?: string;
  source: "github" | "updater";
  error?: string;
};

type GitHubRelease = {
  tag_name?: string;
  name?: string;
  body?: string | null;
  published_at?: string;
  html_url?: string;
};

const GITHUB_RELEASES_API = "https://api.github.com/repos/zoefix/neko-route/releases";

export function isTauriRuntime() {
  return typeof (window as any).__TAURI_INTERNALS__ !== "undefined";
}

export function formatBytes(bytes: number) {
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

export function updateStatusLabel(
  t: (key: MsgKey, vars?: Vars) => string,
  status: UpdateStatus,
  progress: UpdateProgress,
) {
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

export function releaseDateText(date?: string) {
  if (!date) return "";
  const parsed = new Date(date);
  if (Number.isNaN(parsed.getTime())) return "";
  return parsed.toLocaleDateString();
}

export async function fetchLatestReleaseNotes() {
  return fetchGitHubRelease(`${GITHUB_RELEASES_API}/latest`);
}

export async function fetchReleaseNotesForVersion(version: string) {
  return fetchGitHubRelease(`${GITHUB_RELEASES_API}/tags/v${version}`);
}

export function releaseNotesFromUpdate(update: Update, error?: string): ReleaseNotes {
  return {
    version: update.version,
    tagName: `v${update.version}`,
    name: `Neko Route v${update.version}`,
    body: update.body?.trim() ?? "",
    publishedAt: update.date,
    source: "updater",
    error,
  };
}

async function fetchGitHubRelease(url: string): Promise<ReleaseNotes> {
  const response = await fetch(url, {
    headers: {
      Accept: "application/vnd.github+json",
    },
  });

  if (!response.ok) {
    throw new Error(`GitHub Releases ${response.status}`);
  }

  const data = (await response.json()) as GitHubRelease;
  const tagName = data.tag_name ?? "";
  const version = tagName.replace(/^v/, "");
  return {
    version,
    tagName,
    name: data.name || tagName || "Neko Route",
    body: data.body?.trim() ?? "",
    publishedAt: data.published_at,
    url: data.html_url,
    source: "github",
  };
}
