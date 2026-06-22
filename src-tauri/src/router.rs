use crate::{
    codex_alias,
    types::{AppConfig, ModelEntry, Provider},
};

pub const ROUTE_REASON_DIRECT: &str = "direct";
pub const ROUTE_REASON_CODEX_SLOT: &str = "codex_slot";
pub const ROUTE_REASON_CODEX_INTERNAL_LOCKED: &str = "codex_internal_locked";
pub const ROUTE_REASON_FALLBACK_UNKNOWN: &str = "fallback_unknown";

#[derive(Debug, Clone)]
pub struct RouteMatch {
    pub model: ModelEntry,
    pub provider: Provider,
    pub upstream_model: String,
    pub timeout_ms: u64,
    pub retry_count: u8,
    pub requested_model: String,
    pub route_reason: String,
    pub locked_from_model: Option<String>,
}

pub fn match_route(config: &AppConfig, model_id: &str) -> Result<RouteMatch, String> {
    let requested = model_id.trim();
    if requested.is_empty() {
        return Err("Missing model".into());
    }
    let route = resolve_route_model(config, requested)?;
    route_match_from_model(
        config,
        requested,
        route.model,
        route.reason,
        route.locked_from_model,
    )
}

/// Provider-scoped resolver used by the model-test commands. Unlike
/// [`match_route`], this intentionally matches disabled models as well, so the
/// UI can test a model before enabling it.
pub fn match_route_for_provider(
    config: &AppConfig,
    model_id: &str,
    provider_id: &str,
) -> Result<RouteMatch, String> {
    let requested = model_id.trim();
    if requested.is_empty() {
        return Err("Missing model".into());
    }
    let provider_id = provider_id.trim();
    if provider_id.is_empty() {
        return Err("Missing provider".into());
    }
    let model = config
        .models
        .iter()
        .find(|model| model.id == requested && model.provider_id == provider_id)
        .ok_or_else(|| {
            format!("Model '{requested}' is not configured under provider '{provider_id}'")
        })?;
    route_match_from_model(config, requested, model, ROUTE_REASON_DIRECT, None)
}

fn route_match_from_model(
    config: &AppConfig,
    requested: &str,
    model: &ModelEntry,
    route_reason: &str,
    locked_from_model: Option<String>,
) -> Result<RouteMatch, String> {
    let provider = config
        .providers
        .iter()
        .find(|provider| provider.id == model.provider_id)
        .ok_or_else(|| {
            format!(
                "Model '{}' uses provider '{}' but that provider is missing",
                model.id, model.provider_id
            )
        })?;

    Ok(RouteMatch {
        model: model.clone(),
        provider: provider.clone(),
        upstream_model: model
            .upstream_model
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| model.id.clone()),
        timeout_ms: model.timeout_ms,
        retry_count: model.retry_count,
        requested_model: requested.to_string(),
        route_reason: route_reason.to_string(),
        locked_from_model,
    })
}

struct RouteModel<'a> {
    model: &'a ModelEntry,
    reason: &'static str,
    locked_from_model: Option<String>,
}

fn resolve_route_model<'a>(
    config: &'a AppConfig,
    requested: &str,
) -> Result<RouteModel<'a>, String> {
    if let Some(model) = codex_alias::resolve_slot_model(config, requested) {
        return Ok(RouteModel {
            model,
            reason: ROUTE_REASON_CODEX_SLOT,
            locked_from_model: None,
        });
    }

    if config.settings.codex_internal_model_lock && codex_alias::is_codex_internal_model(requested)
    {
        if let Some(model) = resolve_internal_lock_target(config, requested) {
            let locked = model.id != requested;
            return Ok(RouteModel {
                model,
                reason: if locked {
                    ROUTE_REASON_CODEX_INTERNAL_LOCKED
                } else {
                    ROUTE_REASON_DIRECT
                },
                locked_from_model: locked.then(|| requested.to_string()),
            });
        }

        return Err(format!(
            "Codex internal model '{requested}' is locked, but no enabled Codex default or fallback model is configured"
        ));
    }

    if let Some(model) = codex_alias::resolve_direct_model(config, requested) {
        return Ok(RouteModel {
            model,
            reason: ROUTE_REASON_DIRECT,
            locked_from_model: None,
        });
    }

    if let Some(model) = resolve_fallback(config, requested) {
        return Ok(RouteModel {
            model,
            reason: ROUTE_REASON_FALLBACK_UNKNOWN,
            locked_from_model: None,
        });
    }

    Err(format!(
        "Model '{requested}' is not enabled in Model Garden"
    ))
}

fn resolve_internal_lock_target<'a>(
    config: &'a AppConfig,
    requested: &str,
) -> Option<&'a ModelEntry> {
    config
        .settings
        .codex_default_model
        .as_deref()
        .and_then(|model| resolve_configured_model(config, model))
        .or_else(|| resolve_fallback(config, requested))
}

fn resolve_configured_model<'a>(config: &'a AppConfig, model_id: &str) -> Option<&'a ModelEntry> {
    let model_id = model_id.trim();
    if model_id.is_empty() {
        return None;
    }
    codex_alias::resolve_direct_model(config, model_id)
        .or_else(|| codex_alias::resolve_slot_model(config, model_id))
}

/// When a request names an unknown model, redirect it to the configured fallback
/// so unsupported non-Codex slugs do not fail unexpectedly. Codex internal model
/// names are handled before this by the internal-model lock.
fn resolve_fallback<'a>(config: &'a AppConfig, requested: &str) -> Option<&'a ModelEntry> {
    let fallback_id = config
        .settings
        .fallback_model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    if fallback_id == requested {
        return None;
    }
    config
        .models
        .iter()
        .find(|model| model.id == fallback_id && model.enabled)
}

#[cfg(test)]
mod tests {
    use super::{
        match_route, match_route_for_provider, ROUTE_REASON_CODEX_INTERNAL_LOCKED,
        ROUTE_REASON_CODEX_SLOT, ROUTE_REASON_DIRECT, ROUTE_REASON_FALLBACK_UNKNOWN,
    };
    use crate::store::normalize_config;
    use crate::types::{
        default_config, CodexInjectionMode, Provider, ProviderKind, ProviderProtocol,
    };

    #[test]
    fn model_controls_provider_selection() {
        let mut config = default_config();
        config.providers.push(Provider {
            id: "custom-chat".into(),
            name: "Custom Chat".into(),
            kind: ProviderKind::Custom,
            protocol: ProviderProtocol::OpenAiChatCompletions,
            base_url: "https://proxy.example/v1".into(),
            key_ref: Some("provider:custom-chat".into()),
            http_proxy: Default::default(),
        });
        let model = config
            .models
            .iter_mut()
            .find(|model| model.id == "gpt-5.5")
            .unwrap();
        model.provider_id = "custom-chat".into();
        model.upstream_model = Some("gpt-5.5-proxy".into());

        let matched = match_route(&config, "gpt-5.5").unwrap();
        assert_eq!(matched.provider.id, "custom-chat");
        assert_eq!(matched.upstream_model, "gpt-5.5-proxy");
        assert_eq!(matched.route_reason, ROUTE_REASON_DIRECT);
    }

    #[test]
    fn disabled_model_is_not_routable() {
        let mut config = default_config();
        config
            .models
            .iter_mut()
            .find(|model| model.id == "gpt-5.5")
            .unwrap()
            .enabled = false;

        assert!(match_route(&config, "gpt-5.5").is_err());
    }

    #[test]
    fn provider_scoped_match_chooses_duplicate_model_under_requested_provider() {
        let mut config = default_config();
        config.providers.push(Provider {
            id: "openai-account-user".into(),
            name: "User OpenAI".into(),
            kind: ProviderKind::OfficialOpenAiAccount,
            protocol: ProviderProtocol::OpenAiResponses,
            base_url: "https://api.openai.com/v1".into(),
            key_ref: Some("official-token:openai-account-user".into()),
            http_proxy: Default::default(),
        });
        let mut account_model = config
            .models
            .iter()
            .find(|model| model.id == "gpt-5.5")
            .unwrap()
            .clone();
        account_model.provider_id = "openai-account-user".into();
        account_model.display_name = "GPT-5.5 User Account".into();
        config.models.push(account_model);

        let direct = match_route(&config, "gpt-5.5").unwrap();
        assert_eq!(direct.provider.id, "openai-official");

        let scoped = match_route_for_provider(&config, "gpt-5.5", "openai-account-user").unwrap();
        assert_eq!(scoped.provider.id, "openai-account-user");
        assert_eq!(scoped.route_reason, ROUTE_REASON_DIRECT);

        let fixed = match_route_for_provider(&config, "gpt-5.5", "openai-official").unwrap();
        assert_eq!(fixed.provider.kind, ProviderKind::OfficialOpenAi);
    }

    #[test]
    fn provider_scoped_match_does_not_fallback_to_same_named_provider() {
        let config = default_config();
        assert!(match_route_for_provider(&config, "gpt-5.5", "missing-provider").is_err());
    }

    #[test]
    fn provider_scoped_match_allows_disabled_model_for_testing() {
        let mut config = default_config();
        let model = config
            .models
            .iter_mut()
            .find(|model| model.id == "gpt-5.5")
            .unwrap();
        model.enabled = false;
        let provider_id = model.provider_id.clone();

        // The real routing path must still reject a disabled model.
        assert!(match_route(&config, "gpt-5.5").is_err());

        // The provider-scoped resolver used by model testing must still find it.
        let scoped = match_route_for_provider(&config, "gpt-5.5", &provider_id).unwrap();
        assert_eq!(scoped.model.id, "gpt-5.5");
        assert_eq!(scoped.provider.id, provider_id);
        assert_eq!(scoped.route_reason, ROUTE_REASON_DIRECT);
    }

    #[test]
    fn codex_internal_model_routes_to_locked_fallback_when_default_is_unset() {
        let mut config = default_config();
        config.settings.fallback_model = Some("claude-opus-4-8".into());

        let matched = match_route(&config, "gpt-5.4-mini").unwrap();
        assert_eq!(matched.model.id, "claude-opus-4-8");
        assert_eq!(matched.upstream_model, "claude-opus-4-8");
        assert_eq!(matched.route_reason, ROUTE_REASON_CODEX_INTERNAL_LOCKED);
        assert_eq!(matched.locked_from_model.as_deref(), Some("gpt-5.4-mini"));
    }

    #[test]
    fn codex_slot_routes_to_real_model() {
        let mut config = normalize_config(default_config());
        config.settings.codex_injection_mode = CodexInjectionMode::ThirdPartyApi;
        config.settings.codex_default_model = Some("claude-opus-4-8".into());
        config.settings.fallback_model = None;
        let config = normalize_config(config);

        let matched = match_route(&config, "gpt-5.5").unwrap();

        assert_eq!(matched.model.id, "claude-opus-4-8");
        assert_eq!(matched.upstream_model, "claude-opus-4-8");
        assert_eq!(matched.provider.id, "anthropic-cli");
        assert_eq!(matched.route_reason, ROUTE_REASON_CODEX_SLOT);
        assert!(matched.locked_from_model.is_none());
    }

    #[test]
    fn fallback_is_ignored_when_unset() {
        let mut config = default_config();
        config.settings.fallback_model = None;
        assert!(match_route(&config, "gpt-5.4-mini").is_err());
    }

    #[test]
    fn fallback_does_not_mask_itself() {
        let mut config = default_config();
        config.settings.fallback_model = Some("missing-model".into());
        // Fallback target doesn't exist -> original error, no infinite masking.
        assert!(match_route(&config, "gpt-5.4-mini").is_err());
    }

    #[test]
    fn configured_model_ignores_fallback() {
        let mut config = default_config();
        config.settings.fallback_model = Some("claude-opus-4-8".into());
        let matched = match_route(&config, "gpt-5.5").unwrap();
        assert_eq!(matched.model.id, "gpt-5.5");
        assert_eq!(matched.route_reason, ROUTE_REASON_DIRECT);
    }

    #[test]
    fn unknown_non_internal_model_routes_to_fallback() {
        let mut config = default_config();
        config.settings.fallback_model = Some("claude-opus-4-8".into());

        let matched = match_route(&config, "unknown-model").unwrap();

        assert_eq!(matched.model.id, "claude-opus-4-8");
        assert_eq!(matched.route_reason, ROUTE_REASON_FALLBACK_UNKNOWN);
    }

    #[test]
    fn codex_internal_model_lock_uses_codex_default() {
        let mut config = config_with_deepseek_default();
        config.settings.fallback_model = Some("gpt-5.5".into());

        let matched = match_route(&config, "gpt-5.4-mini").unwrap();

        assert_eq!(matched.model.id, "deepseek-v4-pro");
        assert_eq!(matched.upstream_model, "deepseek-chat");
        assert_eq!(matched.provider.id, "deepseek");
        assert_eq!(matched.route_reason, ROUTE_REASON_CODEX_INTERNAL_LOCKED);
    }

    #[test]
    fn configured_internal_model_is_locked_unless_it_is_the_default() {
        let mut config = config_with_deepseek_default();
        let mut internal = config.models[0].clone();
        internal.id = "gpt-5.4".into();
        internal.display_name = "GPT-5.4 Relay".into();
        internal.provider_id = "deepseek".into();
        internal.upstream_model = Some("gpt-5.4-upstream".into());
        config.models.push(internal);

        let locked = match_route(&config, "gpt-5.4").unwrap();
        assert_eq!(locked.model.id, "deepseek-v4-pro");
        assert_eq!(locked.route_reason, ROUTE_REASON_CODEX_INTERNAL_LOCKED);

        config.settings.codex_default_model = Some("gpt-5.4".into());
        let direct = match_route(&config, "gpt-5.4").unwrap();
        assert_eq!(direct.model.id, "gpt-5.4");
        assert_eq!(direct.upstream_model, "gpt-5.4-upstream");
        assert_eq!(direct.route_reason, ROUTE_REASON_DIRECT);
    }

    fn config_with_deepseek_default() -> crate::types::AppConfig {
        let mut config = default_config();
        config.providers.push(Provider {
            id: "deepseek".into(),
            name: "DeepSeek".into(),
            kind: ProviderKind::Custom,
            protocol: ProviderProtocol::OpenAiChatCompletions,
            base_url: "https://deepseek.example/v1".into(),
            key_ref: Some("provider:deepseek".into()),
            http_proxy: Default::default(),
        });
        let mut model = config.models[0].clone();
        model.id = "deepseek-v4-pro".into();
        model.display_name = "DeepSeek V4 Pro".into();
        model.provider_id = "deepseek".into();
        model.upstream_model = Some("deepseek-chat".into());
        model.enabled = true;
        config.models.push(model);
        config.settings.codex_default_model = Some("deepseek-v4-pro".into());
        config
    }
}
