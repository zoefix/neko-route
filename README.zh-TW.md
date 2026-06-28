<div align="center">

# Neko Route

面向 Codex 的本地 AI 路由器。它提供桌面控制面板，用於模型路由、官方帳號接入、服務商管理、請求觀測和 Codex 模型目錄生成。

[English](README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [Changelog](CHANGELOG.md)

![版本](https://img.shields.io/badge/version-v0.1.8-7c3aed?style=for-the-badge)
![Windows](https://img.shields.io/badge/Windows-supported-0078D4?style=for-the-badge&logo=windows&logoColor=white)
![macOS](https://img.shields.io/badge/macOS-supported-000000?style=for-the-badge&logo=apple&logoColor=white)
![Linux](https://img.shields.io/badge/Linux-supported-FCC624?style=for-the-badge&logo=linux&logoColor=111111)

</div>

## 專案簡介

Neko Route 是為 Codex 設計的本地桌面路由器。它暴露本地 OpenAI 相容入口，生成 Codex 模型目錄和設定，並把請求路由到 OpenAI 官方帳號、Claude 官方來源或第三方 API 服務商。

專案重點關注三件事：

- **服務商解耦**：Codex 只需要指向一個本地端點，具體請求由 Neko Route 根據模型設定分發。
- **執行觀測**：在一個桌面應用程式裡查看請求歷史、路由結果、Token 用量、串流狀態、訂閱狀態和服務商額度。
- **本地優先的憑證處理**：Token 和 API key 不寫入 Codex 模型目錄，敏感值透過應用程式自身的憑證儲存路徑保存。

## 產品預覽

<p align="center">
  <img src="docs/images/neko-route-providers.png" alt="Neko Route 服務商管理" width="100%">
</p>

<p align="center">
  <img src="docs/images/neko-route-models.png" alt="Neko Route 模型管理" width="100%">
</p>

## 核心能力

- 為 Codex 提供本地 `/v1/responses` 路由，並支援按模型選擇服務商。
- 匯出 Codex 模型目錄，支援文字和圖片輸入宣告。
- OpenAI 官方帳號支援 Codex 相容 OAuth 和 Codex JSON。
- Claude 官方帳號支援手動 OAuth、Cookie 輔助 OAuth 和 Claude JSON。
- 支援識別 Claude Code CLI 與 Claude Desktop 的本地官方憑證。
- 支援第三方 OpenAI Responses、OpenAI Chat Completions 和 Anthropic Messages 協定。
- 支援重複模型 ID 保護、預設模型修復、兜底模型路由和停用模型排序。
- 請求日誌展示實際服務商、串流狀態、Token 用量、費用估算和最終延遲徽章。
- 基於 GitHub Releases 的應用程式更新，並在應用程式內展示 Release 更新內容。
- 桌面端單一實例執行，重複啟動時喚醒既有視窗。

## 架構

Neko Route 使用 Tauri 2 建構，後端為 Rust，前端為 React。

| 層級 | 職責 |
| --- | --- |
| 桌面應用程式 | 服務商設定、模型管理、Codex 設定、更新、請求日誌 |
| 本地路由 | Codex 呼叫的 OpenAI 相容端點 |
| 服務商適配 | OpenAI Responses、OpenAI Chat Completions、Anthropic Messages、官方帳號路由 |
| 目錄匯出 | Codex 模型目錄和設定生成 |
| 憑證儲存 | 服務商 Token、API key、官方帳號 token JSON |

Codex 存取本地路由。Neko Route 根據請求的模型 ID 匹配服務商，在需要時改寫上游模型名稱，並為相容協定保留圖片、檔案等輸入內容。

## 服務商模型

Neko Route 把官方帳號來源和通用 API 服務商分開處理。

| 來源 | 用途 |
| --- | --- |
| OpenAI 官方帳號 | 透過 OpenAI 帳號 token 路由，並顯示訂閱和額度資訊 |
| Claude 官方帳號 | 透過 Claude 帳號 token 路由，並在可用時顯示訂閱和使用量 |
| Claude Code CLI 官方 | 使用本機 Claude Code CLI 憑證 |
| Claude Desktop 官方 | 使用本機 Claude Desktop 憑證 |
| 第三方 API | 路由到自訂 OpenAI 相容或 Anthropic 相容服務商 |

## 模型目錄

模型以 Codex 可見條目的形式定義。模型可以直接使用上游模型名稱，也可以保持一個穩定的 Codex 可見 ID，同時指向不同的上游模型名稱。

同一模型 ID 同一時間只允許啟用一個條目。這樣可以避免 Codex 模型目錄衝突，同時保留多個同 ID 設定。

## 觀測能力

Dashboard 和日誌頁面用於檢查真實路由情況：

- Codex 請求的模型和實際匹配的服務商。
- 請求完成狀態和串流狀態。
- Prompt、Completion、Cache 和總 Token 用量。
- 最終延遲徽章。
- 支援的官方來源會顯示服務商額度和訂閱卡片。
- 服務商拒絕請求時展示清晰的上游錯誤。

## 安全邊界

Neko Route 是本地控制面板。它的目標是避免把服務商憑證寫進生成給 Codex 的模型目錄和設定裡。

敏感值透過應用程式的憑證儲存流程保存。如果平台鑰匙圈無法保存某些值，Neko Route 會降級到本地 token 儲存路徑，並在後端做嚴格處理。

## 發布

安裝包透過 [GitHub Releases](https://github.com/zoefix/neko-route/releases) 發布。桌面更新器從 GitHub 讀取 Release 資訊，並在安裝前展示目前版本更新內容。

macOS 也可以透過 Homebrew 安裝：

```bash
brew install --cask zoefix/neko-route/neko-route
```

更新 Homebrew 安裝的版本：

```bash
brew update && brew upgrade --cask neko-route
```

支援的桌面平台：

- Windows
- macOS
- Linux

## 開發

常用本地檢查：

```bash
corepack pnpm build
cargo test --manifest-path src-tauri/Cargo.toml
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
git diff --check
```

Windows 安裝包只在明確需要安裝包產物時，透過專案發布或建構腳本生成。

---

## 友情連結

- [LINUX DO — 新的理想型社區](https://linux.do/)
