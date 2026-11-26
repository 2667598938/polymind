//! 供应商注册表与模型引用解析。
//!
//! 模型引用统一写成 `provider_id/model_name`，例如：
//! `openai/gpt-4o`、`deepseek/deepseek-v3.2`、`claude/claude-sonnet-4-5`。
//! 只写模型名时会回退到"唯一持有该模型的供应商"，多供应商同名时明确报错，
//! 绝不在用户不知情的情况下把请求打到另一家。

use std::collections::HashMap;
use std::sync::Arc;

use super::{AnthropicProvider, GeminiProvider, OpenAiProvider, Provider, ProviderInfo, ProviderKind};
use crate::config::ProviderConfig;
use crate::model::{ModelCard, Pricing};
use crate::{Error, Result};

#[derive(Default)]
pub struct Registry {
    providers: HashMap<String, Arc<dyn Provider>>,
    default_ref: Option<String>,
}

impl Registry {
    pub fn new() -> Self {
        Registry::default()
    }

    pub fn register(&mut self, provider: Arc<dyn Provider>) {
        self.providers.insert(provider.info().id.clone(), provider);
    }

    pub fn set_default(&mut self, model_ref: impl Into<String>) {
        self.default_ref = Some(model_ref.into());
    }

    pub fn default_ref(&self) -> Option<&str> {
        self.default_ref.as_deref()
    }

    pub fn from_configs(configs: &[ProviderConfig]) -> Result<Self> {
        let mut reg = Registry::new();
        for c in configs.iter().filter(|c| c.enabled) {
            reg.register(build_provider(c)?);
        }
        if reg.providers.is_empty() {
            return Err(Error::Config("尚未配置任何可用供应商".into()));
        }
        Ok(reg)
    }

    pub fn get(&self, id: &str) -> Result<Arc<dyn Provider>> {
        self.providers
            .get(id)
            .cloned()
            .ok_or_else(|| Error::Config(format!("供应商 `{id}` 不存在或未启用")))
    }

    pub fn list(&self) -> Vec<ProviderInfo> {
        let mut v: Vec<ProviderInfo> = self.providers.values().map(|p| p.info()).collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    pub fn ids(&self) -> Vec<String> {
        let mut v: Vec<String> = self.providers.keys().cloned().collect();
        v.sort();
        v
    }

    /// 解析 `provider/model` 或裸模型名
    pub fn resolve(&self, model_ref: &str) -> Result<(Arc<dyn Provider>, String)> {
        let trimmed = model_ref.trim();
        if trimmed.is_empty() {
            return Err(Error::Config("模型引用为空".into()));
        }
        if let Some((pid, model)) = trimmed.split_once('/') {
            if model.is_empty() {
                return Err(Error::Config(format!("模型引用 `{trimmed}` 缺少模型名")));
            }
            return Ok((self.get(pid)?, model.to_string()));
        }
        // 裸模型名：先看默认供应商
        if let Some(def) = self.default_ref.as_deref() {
            if let Some((pid, model)) = def.split_once('/') {
                if model == trimmed {
                    if let Ok(p) = self.get(pid) {
                        return Ok((p, trimmed.to_string()));
                    }
                }
            }
        }
        let hits: Vec<Arc<dyn Provider>> = self
            .providers
            .values()
            .filter(|p| {
                let info = p.info();
                info.default_model == trimmed || info.models.iter().any(|m| m.id == trimmed)
            })
            .cloned()
            .collect();
        match hits.len() {
            1 => Ok((hits[0].clone(), trimmed.to_string())),
            0 if self.providers.len() == 1 => {
                let only = self.providers.values().next().expect("非空").clone();
                Ok((only, trimmed.to_string()))
            }
            0 => Err(Error::Config(format!("没有供应商声明模型 `{trimmed}`，请改用 provider/model 写法"))),
            _ => Err(Error::Config(format!(
                "模型 `{trimmed}` 被多家供应商声明，请改用 provider/model 写法"
            ))),
        }
    }

    pub fn default_target(&self) -> Result<(Arc<dyn Provider>, String)> {
        if let Some(r) = self.default_ref.clone() {
            return self.resolve(&r);
        }
        let first = self
            .providers
            .values()
            .min_by(|a, b| a.info().id.cmp(&b.info().id))
            .cloned()
            .ok_or_else(|| Error::Config("尚未配置任何可用供应商".into()))?;
        let model = first.info().default_model;
        Ok((first, model))
    }

    /// 自动发现：逐个供应商 try，返回第一个连通的
    pub async fn discover_first_healthy(&self) -> Option<(String, String)> {
        for id in self.ids() {
            if let Ok(p) = self.get(&id) {
                if p.health().await.is_ok() {
                    let info = p.info();
                    return Some((id, info.default_model));
                }
            }
        }
        None
    }
}

pub fn build_provider(c: &ProviderConfig) -> Result<Arc<dyn Provider>> {
    let models = models_from_config(c);
    let base = if c.base_url.trim().is_empty() { c.kind.default_base_url().to_string() } else { c.base_url.clone() };
    let p: Arc<dyn Provider> = match c.kind {
        ProviderKind::OpenAi | ProviderKind::AzureOpenAi | ProviderKind::Ollama => Arc::new(OpenAiProvider::new(
            c.id.clone(),
            if c.label.is_empty() { c.kind.label().to_string() } else { c.label.clone() },
            base,
            c.api_key.clone(),
            c.kind,
            c.default_model.clone(),
            models,
            c.pricing.clone(),
        )),
        ProviderKind::Anthropic => Arc::new(AnthropicProvider::new(
            c.id.clone(),
            if c.label.is_empty() { c.kind.label().to_string() } else { c.label.clone() },
            base,
            c.api_key.clone(),
            c.default_model.clone(),
            models,
            c.pricing.clone(),
        )),
        ProviderKind::Gemini => Arc::new(GeminiProvider::new(
            c.id.clone(),
            if c.label.is_empty() { c.kind.label().to_string() } else { c.label.clone() },
            base,
            c.api_key.clone(),
            c.default_model.clone(),
            models,
            c.pricing.clone(),
        )),
    };
    Ok(p)
}

fn models_from_config(c: &ProviderConfig) -> Vec<ModelCard> {
    c.models
        .iter()
        .map(|m| ModelCard {
            id: m.clone(),
            display_name: m.clone(),
            context_window: 0,
            supports_tools: true,
            supports_vision: true,
            supports_reasoning: true,
        })
        .collect()
}

/// 从一份预设清单快速造出 `ModelCard`（UI 的"推荐模型"下拉用）
pub fn preset_models(pairs: &[(&str, u32, bool)]) -> Vec<ModelCard> {
    pairs
        .iter()
        .map(|(id, ctx, tools)| ModelCard {
            id: id.to_string(),
            display_name: id.to_string(),
            context_window: *ctx,
            supports_tools: *tools,
            supports_vision: true,
            supports_reasoning: true,
        })
        .collect()
}

#[allow(dead_code)]
pub fn default_pricing_for(kind: ProviderKind) -> Pricing {
    match kind {
        ProviderKind::OpenAi => Pricing { input_per_mtok: 2.5, output_per_mtok: 10.0, cache_read_per_mtok: 0.5 },
        ProviderKind::Anthropic => Pricing { input_per_mtok: 3.0, output_per_mtok: 15.0, cache_read_per_mtok: 0.3 },
        ProviderKind::Gemini => Pricing { input_per_mtok: 1.25, output_per_mtok: 5.0, cache_read_per_mtok: 0.31 },
        _ => Pricing::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(id: &str, kind: ProviderKind, model: &str) -> ProviderConfig {
        ProviderConfig {
            id: id.into(),
            kind,
            label: id.into(),
            base_url: "https://example.com".into(),
            api_key: "k".into(),
            api_key_cipher: String::new(),
            default_model: model.into(),
            models: vec![model.into()],
            pricing: Pricing::default(),
            enabled: true,
        }
    }

    #[test]
    fn resolves_explicit_and_bare_refs() {
        let reg = Registry::from_configs(&[cfg("openai", ProviderKind::OpenAi, "gpt-4o"), cfg("claude", ProviderKind::Anthropic, "claude-sonnet-4-5")]).unwrap();
        let (p, m) = reg.resolve("openai/gpt-4o").unwrap();
        assert_eq!(p.info().id, "openai");
        assert_eq!(m, "gpt-4o");
        let (p, _) = reg.resolve("claude-sonnet-4-5").unwrap();
        assert_eq!(p.info().id, "claude");
    }

    #[test]
    fn ambiguous_bare_model_is_rejected() {
        let reg = Registry::from_configs(&[cfg("a", ProviderKind::OpenAi, "m"), cfg("b", ProviderKind::OpenAi, "m")]).unwrap();
        assert!(reg.resolve("m").is_err());
    }

    #[test]
    fn unknown_provider_is_rejected() {
        let reg = Registry::from_configs(&[cfg("openai", ProviderKind::OpenAi, "gpt-4o")]).unwrap();
        assert!(reg.resolve("nope/gpt-4o").is_err());
    }
}
