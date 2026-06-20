use crate::types::{AppConfig, CodexInjectionMode, ModelEntry};
use std::collections::{HashMap, HashSet};

const VISIBLE_THIRD_PARTY_SLUGS: &[&str] = &["gpt-5.5", "gpt-5.4"];
const THIRD_PARTY_ALIAS_POOL: &[&str] = &["gpt-5.4-mini", "gpt-5.3-codex", "gpt-5.2"];
pub const CODEX_INTERNAL_MODEL_SLUGS: &[&str] = &[
    "gpt-5.4-mini",
    "gpt-5.4",
    "gpt-5.3-codex",
    "gpt-5.2-codex",
    "gpt-5.2",
    "gpt-4.1-mini",
];

pub fn is_codex_visible_slug(slug: &str) -> bool {
    VISIBLE_THIRD_PARTY_SLUGS.contains(&slug) || THIRD_PARTY_ALIAS_POOL.contains(&slug)
}

pub fn is_codex_internal_model(slug: &str) -> bool {
    CODEX_INTERNAL_MODEL_SLUGS
        .iter()
        .any(|internal| internal.eq_ignore_ascii_case(slug.trim()))
}

pub fn normalize_model_aliases(models: &mut [ModelEntry]) {
    let real_ids = models
        .iter()
        .map(|model| model.id.clone())
        .collect::<HashSet<_>>();
    let mut used = real_ids.clone();

    for model in models {
        if is_codex_visible_slug(&model.id) {
            model.codex_alias = None;
            continue;
        }

        let current = model
            .codex_alias
            .as_deref()
            .map(str::trim)
            .filter(|alias| {
                is_codex_visible_slug(alias) && !alias.is_empty() && !used.contains(*alias)
            })
            .map(str::to_string);

        if let Some(alias) = current {
            used.insert(alias.clone());
            model.codex_alias = Some(alias);
            continue;
        }

        model.codex_alias = THIRD_PARTY_ALIAS_POOL
            .iter()
            .find(|alias| !used.contains(**alias))
            .map(|alias| {
                used.insert((*alias).to_string());
                (*alias).to_string()
            });
    }
}

pub fn export_slug_map<'a>(
    config: &AppConfig,
    models: &[&'a ModelEntry],
) -> Result<HashMap<String, String>, String> {
    if config.settings.codex_injection_mode != CodexInjectionMode::ThirdPartyApi {
        return models
            .iter()
            .map(|model| Ok((model.id.clone(), model.id.clone())))
            .collect();
    }

    let real_ids = config
        .models
        .iter()
        .map(|model| model.id.clone())
        .collect::<HashSet<_>>();
    let mut used = HashSet::new();
    let mut mapped = HashMap::new();

    for model in models {
        let slug = if is_codex_visible_slug(&model.id) {
            model.id.clone()
        } else if let Some(alias) = model.codex_alias.as_deref().map(str::trim).filter(|alias| {
            is_codex_visible_slug(alias) && !real_ids.contains(*alias) && !used.contains(*alias)
        }) {
            alias.to_string()
        } else {
            THIRD_PARTY_ALIAS_POOL
                .iter()
                .find(|alias| !real_ids.contains(**alias) && !used.contains(**alias))
                .map(|alias| (*alias).to_string())
                .ok_or_else(|| {
                    format!(
                        "Model '{}' needs a Codex-compatible menu alias for third-party API injection",
                        model.id
                    )
                })?
        };

        if !used.insert(slug.clone()) {
            return Err(format!(
                "Codex catalog slug '{slug}' is used by more than one model"
            ));
        }
        mapped.insert(model.id.clone(), slug);
    }

    Ok(mapped)
}

pub fn resolve_direct_model<'a>(config: &'a AppConfig, requested: &str) -> Option<&'a ModelEntry> {
    config
        .models
        .iter()
        .find(|model| model.id == requested && model.enabled)
}

pub fn resolve_alias_model<'a>(config: &'a AppConfig, requested: &str) -> Option<&'a ModelEntry> {
    if config.settings.codex_injection_mode != CodexInjectionMode::ThirdPartyApi {
        return None;
    }
    config
        .models
        .iter()
        .find(|model| model.enabled && model.codex_alias.as_deref() == Some(requested))
}

#[cfg(test)]
mod tests {
    use super::normalize_model_aliases;
    use crate::types::default_config;

    #[test]
    fn assigns_codex_alias_to_non_visible_models() {
        let mut config = default_config();
        normalize_model_aliases(&mut config.models);

        let claude = config
            .models
            .iter()
            .find(|model| model.id == "claude-opus-4-8")
            .unwrap();
        assert_eq!(claude.codex_alias.as_deref(), Some("gpt-5.4-mini"));
    }

    #[test]
    fn alias_pool_skips_real_model_ids() {
        let mut config = default_config();
        config.models[1].id = "gpt-5.4-mini".into();
        normalize_model_aliases(&mut config.models);

        let sonnet = config
            .models
            .iter()
            .find(|model| model.id == "claude-sonnet-4-5")
            .unwrap();
        assert_eq!(sonnet.codex_alias.as_deref(), Some("gpt-5.3-codex"));
    }
}
