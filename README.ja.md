<div align="center">

# Neko Route

Codex 向けのローカル AI ルーター。モデルルーティング、公式アカウント接続、プロバイダー管理、リクエスト観測、Codex モデルカタログ生成を扱うデスクトップコントロールプレーンです。

[English](README.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [日本語](README.ja.md) | [Changelog](CHANGELOG.md)

![Version](https://img.shields.io/badge/version-v0.1.8-7c3aed?style=for-the-badge)
![Windows](https://img.shields.io/badge/Windows-supported-0078D4?style=for-the-badge&logo=windows&logoColor=white)
![macOS](https://img.shields.io/badge/macOS-supported-000000?style=for-the-badge&logo=apple&logoColor=white)
![Linux](https://img.shields.io/badge/Linux-supported-FCC624?style=for-the-badge&logo=linux&logoColor=111111)

</div>

## 概要

Neko Route は Codex のために設計されたローカルデスクトップルーターです。ローカルの OpenAI 互換エントリーポイントを公開し、Codex のモデルカタログと設定を生成し、リクエストを OpenAI 公式アカウント、Claude 公式ソース、またはサードパーティ API プロバイダーへルーティングします。

このプロジェクトは次の 3 点に重点を置いています。

- **プロバイダーの分離**：Codex は 1 つのローカルエンドポイントだけを参照し、実際のルーティングは Neko Route がモデル設定に基づいて決定します。
- **運用の可視化**：リクエスト履歴、ルーティング結果、Token 使用量、ストリーム状態、サブスクリプション、プロバイダー使用量を 1 つのデスクトップアプリで確認できます。
- **ローカル優先の認証情報管理**：Token と API key を Codex モデルカタログへ書き込まず、機密値はアプリの認証情報保存経路で扱います。

## プレビュー

<p align="center">
  <img src="docs/images/neko-route-providers.png" alt="Neko Route provider management" width="100%">
</p>

<p align="center">
  <img src="docs/images/neko-route-models.png" alt="Neko Route model management" width="100%">
</p>

## 主な機能

- Codex 向けのローカル `/v1/responses` ルーティングと、モデル単位のプロバイダー選択。
- テキストおよび画像入力を宣言した Codex モデルカタログの出力。
- Codex 互換 OAuth または Codex JSON による OpenAI 公式アカウント接続。
- 手動 OAuth、Cookie 補助 OAuth、Claude JSON による Claude 公式アカウント接続。
- Claude Code CLI と Claude Desktop のローカル公式認証情報の検出。
- サードパーティの OpenAI Responses、OpenAI Chat Completions、Anthropic Messages プロバイダー対応。
- 重複モデル ID の保護、デフォルトモデル修復、フォールバックモデルルーティング、無効モデルの並び替え。
- 実際のプロバイダー、ストリーム状態、Token 使用量、コスト見積もり、最終遅延バッジを含むリクエストログ。
- GitHub Releases ベースのアプリ更新と、アプリ内でのリリースノート表示。
- デスクトップの単一インスタンス動作と、重複起動時の既存ウィンドウのフォーカス。

## アーキテクチャ

Neko Route は Tauri 2 で構築され、バックエンドは Rust、フロントエンドは React です。

| レイヤー | 役割 |
| --- | --- |
| デスクトップアプリ | プロバイダー設定、モデル管理、Codex 設定、更新、リクエストログ |
| ローカルルーター | Codex が呼び出す OpenAI 互換エンドポイント |
| プロバイダーアダプター | OpenAI Responses、OpenAI Chat Completions、Anthropic Messages、公式アカウントルート |
| カタログ出力 | Codex モデルカタログと設定の生成 |
| 認証情報保存 | プロバイダー Token、API key、公式アカウント token JSON |

Codex はローカルルーターにアクセスします。Neko Route は要求されたモデル ID を設定済みのプロバイダーへ対応付け、必要に応じて上流モデル名を書き換え、対応プロトコルでは画像やファイルなどの入力を保持します。

## プロバイダーモデル

Neko Route は公式アカウントソースと汎用 API プロバイダーを分離して扱います。

| ソース | 用途 |
| --- | --- |
| OpenAI Official Account | OpenAI アカウント token でルーティングし、サブスクリプションと使用量を表示 |
| Claude Official Account | Claude アカウント token でルーティングし、利用可能な場合はサブスクリプションと使用量を表示 |
| Claude Code CLI Official | ローカルの Claude Code CLI 認証情報を使用 |
| Claude Desktop Official | ローカルの Claude Desktop 認証情報を使用 |
| Third-party API | カスタム OpenAI 互換または Anthropic 互換プロバイダーへルーティング |

## モデルカタログ

モデルは Codex から見えるエントリーとして定義されます。モデルは上流モデル名をそのまま使うことも、安定した Codex 表示 ID を保ったまま別の上流モデル名へ向けることもできます。

同じモデル ID のエントリーは同時に 1 つだけ有効化できます。これにより Codex モデルカタログの衝突を避けながら、同じ ID の複数設定を保存できます。

## 観測性

Dashboard とログページは実際のルーティング確認に使います。

- Codex が要求したモデルと実際に一致したプロバイダー。
- リクエスト完了状態とストリーム状態。
- Prompt、Completion、Cache、Total の Token 使用量。
- 最終遅延バッジ。
- 対応する公式ソースのプロバイダー使用量とサブスクリプションカード。
- プロバイダーが拒否した場合の明確な上流エラー。

## セキュリティ境界

Neko Route はローカルコントロールプレーンアプリです。生成される Codex モデルカタログと設定にプロバイダー認証情報を書き込まないことを目的としています。

機密値はアプリの認証情報保存フローで保存されます。プラットフォームのキーチェーンが値を保存できない場合、Neko Route はローカル token 保存経路へフォールバックし、バックエンドで厳密に扱います。

## リリース

インストーラーは [GitHub Releases](https://github.com/zoefix/neko-route/releases) で公開されます。デスクトップアップデーターは GitHub から Release 情報を読み取り、インストール前に現在のリリースノートをアプリ内で表示します。

macOS は Homebrew でもインストールできます。

```bash
brew install --cask zoefix/neko-route/neko-route
```

Homebrew 版を更新するには、次のコマンドを実行します。

```bash
brew update && brew upgrade --cask neko-route
```

対応デスクトッププラットフォーム：

- Windows
- macOS
- Linux

## 開発

一般的なローカルチェック：

```bash
corepack pnpm build
cargo test --manifest-path src-tauri/Cargo.toml
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
git diff --check
```

Windows インストーラーは、インストーラー成果物が必要な場合にのみ、プロジェクトのリリースまたはビルドスクリプトで生成します。
