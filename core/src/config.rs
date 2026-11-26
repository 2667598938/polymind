//! 应用配置：一份 human-editable 的 JSON。
//!
//! 明文 API Key 只存在于内存（`api_key` 字段被 serde skip），
//! 落盘的是 `api_key_cipher`，由 `SecretBox` 解密后回填。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use crate::agent::permission::ApprovalMode;
use crate::model::Pricing;
use crate::provider::ProviderKind;
use crate::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub id: String,
    pub kind: ProviderKind,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub base_url: String,
    /// 内存明文，永不写盘
    #[serde(skip)]
    pub api_key: String,
    /// 写盘密文
    #[serde(default)]
    pub api_key_cipher: String,
    pub default_model: String,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub pricing: Pricing,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub auto_start: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub version: u32,
    /// `provider/model`，例如 `openai/gpt-4o`
    pub default_model: String,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
    #[serde(default)]
    pub approval_mode: ApprovalMode,
    /// 工具可读写的工作区根目录（绝对路径）
    #[serde(default)]
    pub allowed_roots: Vec<String>,
    /// 允许执行的命令可执行名（不带路径，跨平台比较时忽略 .exe/.cmd）
    #[serde(default)]
    pub command_allowlist: Vec<String>,
    #[serde(default = "default_steps")]
    pub max_steps: usize,
    #[serde(default = "default_budget")]
    pub context_budget_tokens: usize,
    /// 主模型失败时的兜底模型链
    #[serde(default)]
    pub fallback_models: Vec<String>,
    #[serde(default)]
    pub system_prompt_extra: String,
    #[serde(default = "default_theme")]
    pub theme: String,
}

fn default_steps() -> usize {
    16
}
fn default_budget() -> usize {
    96_000
}
fn default_theme() -> String {
    "graphite".into()
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            version: 1,
            default_model: String::new(),
            providers: vec![],
            mcp_servers: vec![],
            approval_mode: ApprovalMode::Ask,
            allowed_roots: vec![],
            command_allowlist: vec![
                "git".into(),
                "node".into(),
                "npm".into(),
                "pnpm".into(),
                "yarn".into(),
                "cargo".into(),
                "rustc".into(),
                "python".into(),
                "python3".into(),
                "pip".into(),
                "pytest".into(),
                "rg".into(),
                "docker".into(),
            ],
            max_steps: default_steps(),
            context_budget_tokens: default_budget(),
            fallback_models: vec![],
            system_prompt_extra: String::new(),
            theme: default_theme(),
        }
    }
}

impl AppConfig {
    pub fn load_or_init(path: &Path) -> Result<Self> {
        if !path.exists() {
            let mut cfg = AppConfig::default();
            cfg.allowed_roots = std::env::current_dir()
                .ok()
                .map(|d| vec![d.to_string_lossy().to_string()])
                .unwrap_or_default();
            cfg.save(path)?;
            return Ok(cfg);
        }
        let raw = std::fs::read_to_string(path)?;
        let cfg: AppConfig = serde_json::from_str(&raw).map_err(|e| Error::Config(format!("配置解析失败 {}: {e}", path.display())))?;
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        // 先写临时文件再原子替换，避免断电导致配置半截
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn provider(&self, id: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.id == id)
    }

    pub fn provider_mut(&mut self, id: &str) -> Option<&mut ProviderConfig> {
        self.providers.iter_mut().find(|p| p.id == id)
    }

    pub fn upsert_provider(&mut self, p: ProviderConfig) {
        if let Some(slot) = self.providers.iter_mut().find(|x| x.id == p.id) {
            *slot = p;
        } else {
            self.providers.push(p);
        }
    }

    pub fn upsert_mcp(&mut self, s: McpServerConfig) {
        if let Some(slot) = self.mcp_servers.iter_mut().find(|x| x.name == s.name) {
            *slot = s;
        } else {
            self.mcp_servers.push(s);
        }
    }

    /// 给 UI 的回显副本：不下发明文 key，只给打码尾号
    pub fn redacted(&self) -> serde_json::Value {
        let providers: Vec<serde_json::Value> = self
            .providers
            .iter()
            .map(|p| {
                serde_json::json!({
                    "id": p.id,
                    "kind": p.kind,
                    "label": p.label,
                    "base_url": p.base_url,
                    "default_model": p.default_model,
                    "models": p.models,
                    "enabled": p.enabled,
                    "key_masked": crate::secrets::mask_key(&p.api_key),
                    "has_key": !p.api_key.is_empty(),
                })
            })
            .collect();
        serde_json::json!({
            "version": self.version,
            "default_model": self.default_model,
            "approval_mode": self.approval_mode,
            "allowed_roots": self.allowed_roots,
            "command_allowlist": self.command_allowlist,
            "max_steps": self.max_steps,
            "context_budget_tokens": self.context_budget_tokens,
            "fallback_models": self.fallback_models,
            "system_prompt_extra": self.system_prompt_extra,
            "theme": self.theme,
            "providers": providers,
            "mcp_servers": self.mcp_servers,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn init_then_reload_is_stable() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        let cfg = AppConfig::load_or_init(&path).unwrap();
        assert!(path.exists());
        assert_eq!(cfg.max_steps, 16);
        cfg.save(&path).unwrap();
        let again = AppConfig::load_or_init(&path).unwrap();
        assert_eq!(again.command_allowlist, cfg.command_allowlist);
    }

    #[test]
    fn plaintext_key_never_serialized() {
        let mut cfg = AppConfig::default();
        let mut p = ProviderConfig {
            id: "x".into(),
            kind: ProviderKind::OpenAi,
            label: "X".into(),
            base_url: String::new(),
            api_key: "sk-super-secret".into(),
            api_key_cipher: "v1:cipher".into(),
            default_model: "m".into(),
            models: vec!["m".into()],
            pricing: Pricing::default(),
            enabled: true,
        };
        p.api_key = "sk-super-secret".into();
        cfg.upsert_provider(p);
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(!json.contains("super-secret"));
        assert!(json.contains("v1:cipher"));
        assert!(!cfg.redacted().to_string().contains("super-secret"));
    }
}
