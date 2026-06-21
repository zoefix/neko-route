# Changelog

All notable changes to Neko Route are documented here.

This changelog is maintained in English.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and version numbers follow the public app releases.

## [Unreleased]

### Added

- Added Homebrew Cask installation and release automation for macOS.

## [0.1.6] - 2026-06-22

### Fixed

- Fixed OpenAI Responses forwarding after switching from DeepSeek by removing Neko Route local reasoning markers before upstream verification.

## [0.1.3] - 2026-06-21

### Added

- Added LAN sharing mode with remote model discovery and Codex configuration support.
- Added Claude context-pressure tracking, context bridge diagnostics, and archived tool-result recall.
- Added request-log stream state tracking for converted Anthropic and Chat Completions streams.
- Added richer Codex catalog metadata and unified auto-compact limits for all model protocols.

### Changed

- Mirrored Claude Desktop / Claude Code Anthropic Messages requests more closely, including separate messages and count-tokens profiles.
- Improved Anthropic Messages conversion for system placement, prompt cache positioning, thinking restoration, and large tool-result compression.
- Improved OpenAI Chat Completions bridging from Responses input, including tool-call pairing, multimodal content, reasoning, response formats, and stream conversion.
- Downgraded unsupported Chat Completions `json_schema` response formats to `json_object` for non-allowlisted providers.
- Raised generated model auto-compact limits to 90% for every protocol.
- Refined provider, model, log, Codex setup, and LAN sharing UI surfaces.

### Fixed

- Fixed Claude context-full requests repeatedly failing once before succeeding after compression.
- Fixed Anthropic mid-conversation `system` messages so they are only placed where Claude accepts them.
- Fixed converted Anthropic streams incorrectly reporting success after upstream interruption or missing `message_stop`.
- Fixed Anthropic `max_tokens` stop reasons so they surface as incomplete Responses results.
- Fixed Chat Completions model tests that could miss returned content or reasoning-only output.
- Fixed provider compatibility issues caused by sending `json_schema` to upstreams that only support `json_object`.

## [0.1.2] - 2026-06-20

### Added

- Added multilingual README files for English, Simplified Chinese, Traditional Chinese, and Japanese.
- Added this English changelog.
- Added official OpenAI account authorization through OAuth links and Codex JSON.
- Added official Claude account authorization through manual OAuth, cookie-assisted OAuth, and Claude JSON.
- Added OpenAI and Claude subscription and quota display for supported official sources.
- Added Claude Code CLI and Claude Desktop official credential usage display.
- Added a custom desktop title bar, native window rounding, and single-instance behavior.
- Added a top-bar Codex Desktop start/restart control.

### Changed

- Marked exported Codex models as text and image capable.
- Kept Codex effective context windows at the configured model size.
- Improved provider, model, update, request log, and About page layouts.
- Reworked request logs to show stream state with final latency badges.
- Moved disabled models to the end of the model list while preserving their original order.
- Updated updater handling to use GitHub Release bodies as release notes.

### Fixed

- Fixed large Claude image requests failing with local `413 Payload Too Large` errors.
- Fixed image conversion for OpenAI Chat Completions and Anthropic Messages routes.
- Fixed duplicate model ID conflicts by allowing only one enabled route per model ID.
- Fixed default and fallback models so they repair automatically when available models change.
- Fixed Windows Codex Desktop restart so it no longer opens black console windows.

## [0.1.1] - 2026-06-20

### Added

- Added GitHub Releases based update checking and in-app update dialogs.
- Added GitHub Release changelog loading for the About page and update dialog.
- Added OpenAI official account authorization by link and Codex JSON.
- Added Claude official account authorization by manual OAuth, Cookie session key, and Claude JSON.
- Added OpenAI and Claude official account subscription and usage display.
- Added Claude Code CLI and Claude Desktop official credential support.
- Added all-model image input support in the Codex model catalog.
- Added duplicate model ID handling so only one route with the same model ID can be enabled at once.
- Added single-instance behavior so opening the app again focuses the existing window.
- Added a Codex restart/start control from the top bar.
- Added request stream state tracking, final latency display, and request log pagination.

### Changed

- Improved Codex catalog export for image-capable models and full effective context windows.
- Improved default and fallback model selection so they stay valid when models are enabled, disabled, or removed.
- Improved provider list layout for official account subscriptions, quotas, and local token usage.
- Improved custom title bar behavior and native window rounding on desktop platforms.
- Improved Windows release scripts and updater manifest generation.

### Fixed

- Fixed large image requests returning local `413 Payload Too Large` on `/v1/responses`.
- Fixed image conversion for OpenAI Chat Completions and Anthropic Messages routes.
- Fixed updater notes showing platform artifact text instead of release notes.
- Fixed model list sorting so disabled models are shown after enabled models without losing their original order.
- Fixed request tables so the separate latency column is removed and stream status shows final latency as a badge.

## [0.1.0] - 2026-06-20

### Added

- Initial Neko Route desktop app.
- Added local Codex-compatible routing through a loopback server.
- Added provider management for official sources and third-party APIs.
- Added model management with context window, reasoning level, upstream model, and enable controls.
- Added Codex setup tools for model catalog export and Codex config application.
- Added request dashboard, request logs, token usage, and provider usage views.
- Added local credential storage for provider keys and official account tokens.
- Added macOS and Windows packaging support through Tauri.
