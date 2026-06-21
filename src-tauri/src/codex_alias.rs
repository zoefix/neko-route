use crate::types::{AppConfig, CodexInjectionMode, CodexSlotAssignment, ModelEntry, ProviderKind};
use std::collections::{HashMap, HashSet};

pub const CODEX_SLOT_POOL: &[&str] = &[
    "gpt-5.5",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.3-codex",
    "gpt-5.2-codex",
    "gpt-5.2",
    "gpt-4.1-mini",
];

pub const CODEX_INTERNAL_MODEL_SLUGS: &[&str] = &[
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.3-codex",
    "gpt-5.2-codex",
    "gpt-5.2",
    "gpt-4.1-mini",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexSlotTarget {
    pub source: String,
    pub target_model_id: String,
    pub preferred_slot: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexSlotAllocation {
    pub assignments: Vec<CodexSlotAssignment>,
    pub slots_by_target: HashMap<String, String>,
}

pub fn is_codex_slot(slug: &str) -> bool {
    let slug = slug.trim();
    CODEX_SLOT_POOL
        .iter()
        .any(|slot| slot.eq_ignore_ascii_case(slug))
}

pub fn is_codex_internal_model(slug: &str) -> bool {
    let slug = slug.trim();
    CODEX_INTERNAL_MODEL_SLUGS
        .iter()
        .any(|internal| internal.eq_ignore_ascii_case(slug))
}

pub fn normalize_model_aliases(models: &mut [ModelEntry]) {
    for model in models {
        model.codex_alias = model
            .codex_alias
            .as_deref()
            .map(str::trim)
            .filter(|alias| is_codex_slot(alias))
            .map(str::to_string);
    }
}

pub fn local_slot_source(model: &ModelEntry) -> String {
    format!("local:{}", model.provider_id)
}

pub fn lan_slot_source(settings: &crate::types::Settings) -> String {
    format!(
        "lan:{}:{}",
        settings.lan_remote_host.trim(),
        settings.lan_remote_port
    )
}

pub fn local_slot_targets(
    config: &AppConfig,
    allowed_model_ids: Option<&HashSet<String>>,
) -> Vec<CodexSlotTarget> {
    config
        .models
        .iter()
        .filter(|model| model.enabled)
        .filter(|model| {
            allowed_model_ids
                .map(|allowed| allowed.contains(&model.id))
                .unwrap_or(true)
        })
        .filter(|model| {
            config
                .providers
                .iter()
                .find(|provider| provider.id == model.provider_id)
                .is_some_and(|provider| provider.kind != ProviderKind::OfficialOpenAi)
        })
        .map(|model| CodexSlotTarget {
            source: local_slot_source(model),
            target_model_id: model.id.clone(),
            preferred_slot: preferred_slot_for_model(model),
        })
        .collect()
}

pub fn remote_slot_targets(
    source: &str,
    model_ids: impl IntoIterator<Item = String>,
) -> Vec<CodexSlotTarget> {
    model_ids
        .into_iter()
        .map(|target_model_id| CodexSlotTarget {
            preferred_slot: is_codex_slot(&target_model_id).then(|| target_model_id.clone()),
            source: source.to_string(),
            target_model_id,
        })
        .collect()
}

pub fn allocate_codex_slots(
    mode: CodexInjectionMode,
    existing: &[CodexSlotAssignment],
    targets: &[CodexSlotTarget],
    priority_target_ids: &[String],
) -> CodexSlotAllocation {
    let target_keys = targets
        .iter()
        .map(|target| target_key(&target.source, &target.target_model_id))
        .collect::<HashSet<_>>();
    let targets_by_key = targets
        .iter()
        .map(|target| (target_key(&target.source, &target.target_model_id), target))
        .collect::<HashMap<_, _>>();
    let mut ordered_keys = Vec::<String>::new();

    for target_id in priority_target_ids {
        for target in targets
            .iter()
            .filter(|target| target.target_model_id == *target_id)
        {
            push_unique(
                &mut ordered_keys,
                target_key(&target.source, &target.target_model_id),
            );
        }
    }

    for assignment in existing.iter().filter(|assignment| assignment.mode == mode) {
        let key = target_key(&assignment.source, &assignment.target_model_id);
        if target_keys.contains(&key) {
            push_unique(&mut ordered_keys, key);
        }
    }

    for target in targets {
        push_unique(
            &mut ordered_keys,
            target_key(&target.source, &target.target_model_id),
        );
    }

    let mut used_slots = HashSet::<String>::new();
    let mut assignments = Vec::<CodexSlotAssignment>::new();
    let mut slots_by_target = HashMap::<String, String>::new();

    for key in ordered_keys {
        let Some(target) = targets_by_key.get(&key) else {
            continue;
        };
        let slot = existing
            .iter()
            .find(|assignment| {
                assignment.mode == mode
                    && assignment.source == target.source
                    && assignment.target_model_id == target.target_model_id
                    && is_codex_slot(&assignment.slot)
                    && !used_slots.contains(&assignment.slot)
            })
            .map(|assignment| assignment.slot.clone())
            .or_else(|| {
                target
                    .preferred_slot
                    .as_deref()
                    .filter(|slot| is_codex_slot(slot) && !used_slots.contains(*slot))
                    .map(str::to_string)
            })
            .or_else(|| {
                CODEX_SLOT_POOL
                    .iter()
                    .find(|slot| !used_slots.contains(**slot))
                    .map(|slot| (*slot).to_string())
            });

        let Some(slot) = slot else {
            continue;
        };
        used_slots.insert(slot.clone());
        slots_by_target.insert(target.target_model_id.clone(), slot.clone());
        assignments.push(CodexSlotAssignment {
            mode: mode.clone(),
            source: target.source.clone(),
            slot,
            target_model_id: target.target_model_id.clone(),
        });
    }

    CodexSlotAllocation {
        assignments,
        slots_by_target,
    }
}

pub fn replace_mode_assignments(
    assignments: &mut Vec<CodexSlotAssignment>,
    mode: CodexInjectionMode,
    active_assignments: Vec<CodexSlotAssignment>,
) {
    assignments.retain(|assignment| {
        assignment.mode != mode
            && is_codex_slot(&assignment.slot)
            && !assignment.source.trim().is_empty()
            && !assignment.target_model_id.trim().is_empty()
    });
    assignments.extend(active_assignments);
}

pub fn normalize_local_codex_slots(config: &mut AppConfig) {
    normalize_model_aliases(&mut config.models);
    if config.settings.codex_injection_mode != CodexInjectionMode::ThirdPartyApi {
        config
            .settings
            .codex_slots
            .retain(|assignment| assignment.mode != CodexInjectionMode::ThirdPartyApi);
        return;
    }
    let targets = local_slot_targets(config, None);
    let priority = priority_target_ids(
        config.settings.codex_default_model.as_deref(),
        config.settings.fallback_model.as_deref(),
    );
    let allocation = allocate_codex_slots(
        CodexInjectionMode::ThirdPartyApi,
        &config.settings.codex_slots,
        &targets,
        &priority,
    );
    replace_mode_assignments(
        &mut config.settings.codex_slots,
        CodexInjectionMode::ThirdPartyApi,
        allocation.assignments,
    );
    for model in &mut config.models {
        model.codex_alias = None;
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

    let allowed = models
        .iter()
        .map(|model| model.id.clone())
        .collect::<HashSet<_>>();
    let targets = local_slot_targets(config, Some(&allowed));
    let priority = priority_target_ids(
        config.settings.codex_default_model.as_deref(),
        config.settings.fallback_model.as_deref(),
    );
    let allocation = allocate_codex_slots(
        CodexInjectionMode::ThirdPartyApi,
        &config.settings.codex_slots,
        &targets,
        &priority,
    );
    Ok(allocation.slots_by_target)
}

pub fn resolve_direct_model<'a>(config: &'a AppConfig, requested: &str) -> Option<&'a ModelEntry> {
    config
        .models
        .iter()
        .find(|model| model.id == requested && model.enabled)
}

pub fn resolve_slot_model<'a>(config: &'a AppConfig, requested: &str) -> Option<&'a ModelEntry> {
    if config.settings.codex_injection_mode != CodexInjectionMode::ThirdPartyApi {
        return None;
    }
    let requested = requested.trim();
    let assignment = config.settings.codex_slots.iter().find(|assignment| {
        assignment.mode == CodexInjectionMode::ThirdPartyApi && assignment.slot == requested
    })?;
    config
        .models
        .iter()
        .find(|model| model.enabled && model.id == assignment.target_model_id)
}

pub fn priority_target_ids(
    default_model: Option<&str>,
    fallback_model: Option<&str>,
) -> Vec<String> {
    let mut priority = Vec::new();
    for value in [default_model, fallback_model].into_iter().flatten() {
        let value = value.trim();
        if !value.is_empty() && !priority.iter().any(|existing| existing == value) {
            priority.push(value.to_string());
        }
    }
    priority
}

fn preferred_slot_for_model(model: &ModelEntry) -> Option<String> {
    if is_codex_slot(&model.id) {
        return Some(model.id.clone());
    }
    model
        .codex_alias
        .as_deref()
        .map(str::trim)
        .filter(|alias| is_codex_slot(alias))
        .map(str::to_string)
}

fn target_key(source: &str, target_model_id: &str) -> String {
    format!("{source}\u{1f}{target_model_id}")
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        allocate_codex_slots, local_slot_targets, normalize_local_codex_slots, CodexSlotTarget,
        CODEX_SLOT_POOL,
    };
    use crate::types::{
        default_config, CodexInjectionMode, CodexSlotAssignment, Provider, ProviderKind,
        ProviderProtocol,
    };

    #[test]
    fn assigns_default_and_fallback_first() {
        let mut config = default_config();
        config.settings.codex_injection_mode = CodexInjectionMode::ThirdPartyApi;
        config.settings.codex_default_model = Some("claude-opus-4-8".into());
        config.settings.fallback_model = Some("claude-sonnet-4-5".into());

        normalize_local_codex_slots(&mut config);

        let opus = config
            .settings
            .codex_slots
            .iter()
            .find(|assignment| assignment.target_model_id == "claude-opus-4-8")
            .unwrap();
        let sonnet = config
            .settings
            .codex_slots
            .iter()
            .find(|assignment| assignment.target_model_id == "claude-sonnet-4-5")
            .unwrap();
        assert_eq!(opus.slot, "gpt-5.5");
        assert_eq!(sonnet.slot, "gpt-5.4");
    }

    #[test]
    fn preserves_existing_assignment_and_prunes_stale_targets() {
        let targets = vec![
            CodexSlotTarget {
                source: "local:a".into(),
                target_model_id: "model-a".into(),
                preferred_slot: None,
            },
            CodexSlotTarget {
                source: "local:b".into(),
                target_model_id: "model-b".into(),
                preferred_slot: None,
            },
        ];
        let existing = vec![
            CodexSlotAssignment {
                mode: CodexInjectionMode::ThirdPartyApi,
                source: "local:b".into(),
                slot: "gpt-5.2".into(),
                target_model_id: "model-b".into(),
            },
            CodexSlotAssignment {
                mode: CodexInjectionMode::ThirdPartyApi,
                source: "local:old".into(),
                slot: "gpt-5.4".into(),
                target_model_id: "old".into(),
            },
        ];

        let allocation =
            allocate_codex_slots(CodexInjectionMode::ThirdPartyApi, &existing, &targets, &[]);

        assert_eq!(
            allocation
                .slots_by_target
                .get("model-b")
                .map(String::as_str),
            Some("gpt-5.2")
        );
        assert!(!allocation.slots_by_target.contains_key("old"));
    }

    #[test]
    fn skips_overflow_models_instead_of_erroring() {
        let targets = (0..(CODEX_SLOT_POOL.len() + 2))
            .map(|index| CodexSlotTarget {
                source: "local:test".into(),
                target_model_id: format!("model-{index}"),
                preferred_slot: None,
            })
            .collect::<Vec<_>>();

        let allocation =
            allocate_codex_slots(CodexInjectionMode::ThirdPartyApi, &[], &targets, &[]);

        assert_eq!(allocation.assignments.len(), CODEX_SLOT_POOL.len());
        assert!(!allocation.slots_by_target.contains_key("model-8"));
    }

    #[test]
    fn local_targets_ignore_official_openai_provider() {
        let mut config = default_config();
        config.providers.push(Provider {
            id: "custom-chat".into(),
            name: "Custom Chat".into(),
            kind: ProviderKind::Custom,
            protocol: ProviderProtocol::OpenAiChatCompletions,
            base_url: "https://proxy.example/v1".into(),
            key_ref: None,
            http_proxy: Default::default(),
        });
        let mut model = config.models[0].clone();
        model.id = "deepseek-v4-pro".into();
        model.provider_id = "custom-chat".into();
        config.models.push(model);

        let targets = local_slot_targets(&config, None);

        assert!(targets
            .iter()
            .any(|target| target.target_model_id == "deepseek-v4-pro"));
        assert!(!targets
            .iter()
            .any(|target| target.target_model_id == "gpt-5.5"));
    }
}
