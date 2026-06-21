<div align="center">

# Neko Route

面向 Codex 的本地 AI 路由器。它提供桌面控制面板，用于模型路由、官方账号接入、服务商管理、请求观测和 Codex 模型目录生成。

[English](README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [Changelog](CHANGELOG.md)

![版本](https://img.shields.io/badge/version-v0.1.3-7c3aed?style=for-the-badge)
![Windows](https://img.shields.io/badge/Windows-supported-0078D4?style=for-the-badge&logo=windows&logoColor=white)
![macOS](https://img.shields.io/badge/macOS-supported-000000?style=for-the-badge&logo=apple&logoColor=white)
![Linux](https://img.shields.io/badge/Linux-supported-FCC624?style=for-the-badge&logo=linux&logoColor=111111)

</div>

## 项目简介

Neko Route 是为 Codex 设计的本地桌面路由器。它暴露本地 OpenAI 兼容入口，生成 Codex 模型目录和配置，并把请求路由到 OpenAI 官方账号、Claude 官方来源或第三方 API 服务商。

项目重点关注三件事：

- **服务商解耦**：Codex 只需要指向一个本地端点，具体请求由 Neko Route 根据模型配置分发。
- **运行观测**：在一个桌面应用里查看请求历史、路由结果、Token 用量、流状态、订阅状态和服务商额度。
- **本地优先的凭证处理**：Token 和 API key 不写入 Codex 模型目录，敏感值通过应用自身的凭证存储路径保存。

## 产品预览

<p align="center">
  <img src="docs/images/neko-route-providers.png" alt="Neko Route 服务商管理" width="100%">
</p>

<p align="center">
  <img src="docs/images/neko-route-models.png" alt="Neko Route 模型管理" width="100%">
</p>

## 核心能力

- 为 Codex 提供本地 `/v1/responses` 路由，并支持按模型选择服务商。
- 导出 Codex 模型目录，支持文本和图片输入声明。
- OpenAI 官方账号支持 Codex 兼容 OAuth 和 Codex JSON。
- Claude 官方账号支持手动 OAuth、Cookie 辅助 OAuth 和 Claude JSON。
- 支持识别 Claude Code CLI 与 Claude Desktop 的本地官方凭证。
- 支持第三方 OpenAI Responses、OpenAI Chat Completions 和 Anthropic Messages 协议。
- 支持重复模型 ID 保护、默认模型修复、兜底模型路由和禁用模型排序。
- 请求日志展示实际服务商、流状态、Token 用量、费用估算和最终延迟徽章。
- 基于 GitHub Releases 的应用更新，并在应用内展示 Release 更新内容。
- 桌面端单实例运行，重复启动时唤醒已有窗口。

## 架构

Neko Route 使用 Tauri 2 构建，后端为 Rust，前端为 React。

| 层级 | 职责 |
| --- | --- |
| 桌面应用 | 服务商设置、模型管理、Codex 配置、更新、请求日志 |
| 本地路由 | Codex 调用的 OpenAI 兼容端点 |
| 服务商适配 | OpenAI Responses、OpenAI Chat Completions、Anthropic Messages、官方账号路由 |
| 目录导出 | Codex 模型目录和配置生成 |
| 凭证存储 | 服务商 Token、API key、官方账号 token JSON |

Codex 访问本地路由。Neko Route 根据请求的模型 ID 匹配服务商，在需要时改写上游模型名，并为兼容协议保留图片、文件等输入内容。

## 服务商模型

Neko Route 把官方账号来源和通用 API 服务商分开处理。

| 来源 | 用途 |
| --- | --- |
| OpenAI 官方账号 | 通过 OpenAI 账号 token 路由，并显示订阅和额度信息 |
| Claude 官方账号 | 通过 Claude 账号 token 路由，并在可用时显示订阅和使用量 |
| Claude Code CLI 官方 | 使用本机 Claude Code CLI 凭证 |
| Claude Desktop 官方 | 使用本机 Claude Desktop 凭证 |
| 第三方 API | 路由到自定义 OpenAI 兼容或 Anthropic 兼容服务商 |

## 模型目录

模型以 Codex 可见条目的形式定义。模型可以直接使用上游模型名，也可以保持一个稳定的 Codex 可见 ID，同时指向不同的上游模型名。

同一模型 ID 同一时间只允许启用一个条目。这样可以避免 Codex 模型目录冲突，同时保留多个同 ID 配置。

## 观测能力

Dashboard 和日志页面用于检查真实路由情况：

- Codex 请求的模型和实际匹配的服务商。
- 请求完成状态和流状态。
- Prompt、Completion、Cache 和总 Token 用量。
- 最终延迟徽章。
- 支持的官方来源会显示服务商额度和订阅卡片。
- 服务商拒绝请求时展示清晰的上游错误。

## 安全边界

Neko Route 是本地控制面板。它的目标是避免把服务商凭证写进生成给 Codex 的模型目录和配置里。

敏感值通过应用的凭证存储流程保存。如果平台钥匙串无法保存某些值，Neko Route 会降级到本地 token 存储路径，并在后端做严格处理。

## 发布

安装包通过 [GitHub Releases](https://github.com/zoefix/neko-route/releases) 发布。桌面更新器从 GitHub 读取 Release 信息，并在安装前展示当前版本更新内容。

支持的桌面平台：

- Windows
- macOS
- Linux

## 开发

常用本地检查：

```bash
corepack pnpm build
cargo test --manifest-path src-tauri/Cargo.toml
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
git diff --check
```

Windows 安装包只在明确需要安装包产物时，通过项目发布或构建脚本生成。
