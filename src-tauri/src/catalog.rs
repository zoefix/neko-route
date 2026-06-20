use crate::{
    codex_alias,
    types::{AppConfig, ModelEntry},
};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

pub const CATALOG_FILE_NAME: &str = "neko-route.json";

#[cfg(test)]
pub fn catalog_json(config: &AppConfig) -> Value {
    catalog_json_for_models(config, None).unwrap()
}

pub fn catalog_json_for_models(
    config: &AppConfig,
    allowed_model_ids: Option<&HashSet<String>>,
) -> Result<Value, String> {
    let models = config
        .models
        .iter()
        .filter(|model| {
            model.enabled
                && allowed_model_ids
                    .map(|allowed| allowed.contains(&model.id))
                    .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    let slug_map = codex_alias::export_slug_map(config, &models)?;
    let models = models
        .into_iter()
        .map(|model| {
            let slug = slug_map
                .get(&model.id)
                .ok_or_else(|| format!("Missing Codex catalog slug for model '{}'", model.id))?;
            Ok(model_to_codex_json(model, slug))
        })
        .collect::<Result<Vec<_>, String>>()?;

    Ok(json!({ "models": models }))
}

pub fn export_slug_for_model(
    config: &AppConfig,
    allowed_model_ids: Option<&HashSet<String>>,
    model_id: &str,
) -> Result<String, String> {
    let models = config
        .models
        .iter()
        .filter(|model| {
            model.enabled
                && allowed_model_ids
                    .map(|allowed| allowed.contains(&model.id))
                    .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    let slug_map = codex_alias::export_slug_map(config, &models)?;
    slug_map
        .get(model_id)
        .cloned()
        .ok_or_else(|| format!("Codex model '{model_id}' is not available for export"))
}

pub fn write_catalog(codex_home: &Path, config: &AppConfig) -> Result<PathBuf, String> {
    write_catalog_for_models(codex_home, config, None)
}

pub fn write_catalog_for_models(
    codex_home: &Path,
    config: &AppConfig,
    allowed_model_ids: Option<&HashSet<String>>,
) -> Result<PathBuf, String> {
    let dir = codex_home.join("model-catalogs");
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let path = dir.join(CATALOG_FILE_NAME);
    let content =
        serde_json::to_string_pretty(&catalog_json_for_models(config, allowed_model_ids)?)
            .map_err(|error| error.to_string())?;
    fs::write(&path, content).map_err(|error| error.to_string())?;
    Ok(path)
}

fn model_to_codex_json(model: &ModelEntry, slug: &str) -> Value {
    let supported_reasoning_levels = model
        .supported_reasoning_levels
        .iter()
        .map(|level| {
            json!({
                "effort": level,
                "description": reasoning_description(level),
            })
        })
        .collect::<Vec<_>>();
    let default_reasoning_level = if model.default_reasoning_level.trim().is_empty() {
        "medium"
    } else {
        model.default_reasoning_level.as_str()
    };

    json!({
        "slug": slug,
        "display_name": model.display_name,
        "description": model.description,
        "default_reasoning_level": default_reasoning_level,
        "supported_reasoning_levels": supported_reasoning_levels,
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": true,
        "priority": 50,
        "additional_speed_tiers": [],
        "service_tiers": [],
        "availability_nux": null,
        "upgrade": null,
        "base_instructions": BASE_INSTRUCTIONS,
        "model_messages": {
            "instructions_template": "You are Codex, a coding agent based on GPT-5. You and the user share one workspace, and your job is to collaborate with them until their goal is genuinely handled.\n\n{{ personality }}",
            "instructions_variables": {
                "personality_default": "",
                "personality_pragmatic": ""
            }
        },
        "supports_reasoning_summaries": model.reasoning_enabled,
        "default_reasoning_summary": "none",
        "support_verbosity": false,
        "default_verbosity": "low",
        "apply_patch_tool_type": "freeform",
        "web_search_tool_type": "text_and_image",
        "truncation_policy": {
            "mode": "tokens",
            "limit": 10000
        },
        "supports_parallel_tool_calls": true,
        "supports_image_detail_original": true,
        "context_window": model.context_window,
        "max_context_window": model.context_window,
        "effective_context_window_percent": 95,
        "experimental_supported_tools": [],
        "input_modalities": ["text"],
        "supports_search_tool": false,
        "use_responses_lite": false,
        "auto_compact_token_limit": null
    })
}

const BASE_INSTRUCTIONS: &str = "You are Codex, a coding agent. Follow the user's instructions, inspect the workspace before changing code, and keep tool use precise.";

fn reasoning_description(level: &str) -> &'static str {
    match level {
        "low" => "Fast responses with lighter reasoning",
        "medium" => "Balances speed and reasoning depth for everyday tasks",
        "high" => "Greater reasoning depth for complex tasks",
        "xhigh" => "Extra high reasoning depth for complex tasks",
        "max" => "Maximum reasoning depth for the hardest tasks",
        _ => "Custom reasoning effort",
    }
}

#[cfg(test)]
mod tests {
    use super::{catalog_json, catalog_json_for_models};
    use crate::types::{
        default_config, reasoning_defaults_for_protocol, CodexInjectionMode, Provider,
        ProviderKind, ProviderProtocol,
    };

    #[test]
    fn catalog_contains_enabled_models_with_required_fields() {
        let config = default_config();
        let catalog = catalog_json(&config);
        let models = catalog["models"].as_array().unwrap();
        let gpt = models
            .iter()
            .find(|model| model["slug"] == "gpt-5.5")
            .unwrap();
        assert_eq!(gpt["supported_in_api"], true);
        assert!(gpt.get("base_instructions").is_some());
        assert!(gpt.get("model_messages").is_some());
    }

    #[test]
    fn catalog_marks_claude_reasoning_supported_with_max() {
        let config = default_config();
        let catalog = catalog_json(&config);
        let claude = catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| model["slug"] == "claude-opus-4-8")
            .unwrap();
        let levels = claude["supported_reasoning_levels"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|level| level["effort"].as_str())
            .collect::<Vec<_>>();

        assert_eq!(claude["supports_reasoning_summaries"], true);
        assert_eq!(claude["default_reasoning_level"], "max");
        assert!(levels.contains(&"max"));
    }

    #[test]
    fn catalog_marks_chat_completions_reasoning_supported() {
        let mut config = default_config();
        config.providers.push(Provider {
            id: "deepseek".into(),
            name: "DeepSeek".into(),
            kind: ProviderKind::Custom,
            protocol: ProviderProtocol::OpenAiChatCompletions,
            base_url: "https://deepseek.example/v1".into(),
            enabled: true,
            key_ref: Some("provider:deepseek".into()),
        });
        let mut model = config.models[0].clone();
        model.id = "deepseek-v4-pro".into();
        model.display_name = "DeepSeek V4 Pro".into();
        model.provider_id = "deepseek".into();
        let (enabled, default_level, supported_levels) =
            reasoning_defaults_for_protocol(&ProviderProtocol::OpenAiChatCompletions);
        model.reasoning_enabled = enabled;
        model.default_reasoning_level = default_level;
        model.supported_reasoning_levels = supported_levels;
        config.models.push(model);

        let catalog = catalog_json(&config);
        let deepseek = catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| model["slug"] == "deepseek-v4-pro")
            .unwrap();
        let levels = deepseek["supported_reasoning_levels"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|level| level["effort"].as_str())
            .collect::<Vec<_>>();

        assert_eq!(deepseek["supports_reasoning_summaries"], true);
        assert_eq!(deepseek["default_reasoning_level"], "xhigh");
        assert_eq!(levels, ["low", "medium", "high", "xhigh"]);
    }

    #[test]
    fn third_party_catalog_exports_claude_with_visible_alias() {
        let mut config = default_config();
        config.settings.codex_injection_mode = CodexInjectionMode::ThirdPartyApi;

        let catalog = catalog_json(&config);
        let models = catalog["models"].as_array().unwrap();
        let claude = models
            .iter()
            .find(|model| model["display_name"] == "Claude Opus 4.8")
            .unwrap();

        assert_eq!(claude["slug"], "gpt-5.4-mini");
        assert_eq!(claude["default_reasoning_level"], "max");
    }

    #[test]
    fn third_party_catalog_errors_when_alias_pool_is_exhausted() {
        let mut config = default_config();
        config.settings.codex_injection_mode = CodexInjectionMode::ThirdPartyApi;
        let template = config
            .models
            .iter()
            .find(|model| model.id == "claude-opus-4-8")
            .unwrap()
            .clone();
        for id in ["claude-extra-1", "claude-extra-2"] {
            let mut model = template.clone();
            model.id = id.into();
            model.display_name = id.into();
            model.codex_alias = None;
            config.models.push(model);
        }

        let error = catalog_json_for_models(&config, None).unwrap_err();

        assert!(error.contains("Codex-compatible menu alias"));
    }
}
