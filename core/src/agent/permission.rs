//! 权限门：路径牢笼 + 命令白名单 + 分级审批。
//!
//! 威胁模型：模型可能被提示注入，因此"模型说要读什么"永远不等于"允许读什么"。
//! 三层防线：
//! 1. 路径必须落在用户显式授权的工作区根内（canonicalize 后比较，阻断 `..` 与符号链接）；
//! 2. 可执行程序必须在白名单内，且不经过 shell（无管道、无 `&&`、无变量展开）；
//! 3. 写操作与命令执行按风险等级走人工审批，审批粒度可精确到"本会话内同类放行"。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::config::AppConfig;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    /// 只读工具自动放行，写/命令逐次审批（默认）
    #[default]
    Ask,
    /// 低/中风险放行，高风险（命令执行）审批
    Auto,
    /// 只允许只读工具，其余直接拒绝
    ReadOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub request_id: String,
    pub session_id: String,
    pub tool: String,
    pub args: serde_json::Value,
    pub risk: RiskLevel,
    /// 给用户看的一句话，说明"它到底要干什么"
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    AllowOnce,
    /// 本会话内同类工具后续调用不再询问
    AllowSession,
    Deny,
}

#[async_trait]
pub trait Approver: Send + Sync {
    async fn approve(&self, req: ApprovalRequest) -> Decision;
}

/// 无人值守场景的安全兜底
pub struct DenyAll;

#[async_trait]
impl Approver for DenyAll {
    async fn approve(&self, _req: ApprovalRequest) -> Decision {
        Decision::Deny
    }
}

/// 仅在测试/本地演示中使用
pub struct AllowAll;

#[async_trait]
impl Approver for AllowAll {
    async fn approve(&self, _req: ApprovalRequest) -> Decision {
        Decision::AllowOnce
    }
}

pub struct PermissionGate {
    mode: ApprovalMode,
    workspace: PathBuf,
    roots: Vec<PathBuf>,
    command_allowlist: Vec<String>,
    approver: Arc<dyn Approver>,
    session_grants: Mutex<HashSet<String>>,
}

impl PermissionGate {
    pub fn new(mode: ApprovalMode, workspace: PathBuf, roots: Vec<PathBuf>, command_allowlist: Vec<String>, approver: Arc<dyn Approver>) -> Self {
        let canonical_roots: Vec<PathBuf> = roots
            .into_iter()
            .filter_map(|r| {
                let abs = if r.is_absolute() { r } else { workspace.join(r) };
                abs.canonicalize().ok()
            })
            .collect();
        PermissionGate {
            mode,
            workspace,
            roots: canonical_roots,
            command_allowlist: command_allowlist.iter().map(|c| normalize_program(c)).collect(),
            approver,
            session_grants: Mutex::new(HashSet::new()),
        }
    }

    pub fn from_config(cfg: &AppConfig, workspace: &Path, approver: Arc<dyn Approver>) -> Self {
        PermissionGate::new(
            cfg.approval_mode,
            workspace.to_path_buf(),
            cfg.allowed_roots.iter().map(PathBuf::from).collect(),
            cfg.command_allowlist.clone(),
            approver,
        )
    }

    pub fn mode(&self) -> ApprovalMode {
        self.mode
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// 把任意相对/绝对路径解析成"真实可访问路径"，越界即拒绝。
    ///
    /// 允许"尚不存在的文件/多级新目录"：先找到最近的已存在祖先做 canonicalize，
    /// 再把剩余组件拼回去。`..` 在此过程中被视为非法（必须显式落在根内）。
    pub fn jail(&self, path: &str) -> Result<PathBuf> {
        if self.roots.is_empty() {
            return Err(Error::PermissionDenied("尚未授权任何工作区目录".into()));
        }
        let raw = Path::new(path);
        if raw.as_os_str().is_empty() {
            return Err(Error::PermissionDenied("路径为空".into()));
        }
        let candidate = if raw.is_absolute() { raw.to_path_buf() } else { self.workspace.join(raw) };
        if candidate.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err(Error::PermissionDenied(format!("路径含 `..` 上跳，已拒绝: {path}")));
        }

        let mut existing = candidate.clone();
        loop {
            if existing.exists() {
                break;
            }
            match existing.parent() {
                Some(p) if p != existing => existing = p.to_path_buf(),
                _ => break,
            }
        }
        let canon_existing = existing
            .canonicalize()
            .map_err(|e| Error::PermissionDenied(format!("路径无法解析 {path}: {e}")))?;
        let remainder = candidate.strip_prefix(&existing).unwrap_or(Path::new(""));
        let resolved = remainder.components().fold(canon_existing, |mut acc, comp| {
            match comp {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {}
                other => acc.push(other.as_os_str()),
            }
            acc
        });

        if self.roots.iter().any(|r| resolved.starts_with(r)) {
            Ok(resolved)
        } else {
            Err(Error::PermissionDenied(format!(
                "路径越界：{path} 不在已授权工作区内（{}）",
                self.roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>().join(", ")
            )))
        }
    }

    pub fn check_command(&self, program: &str) -> Result<()> {
        let norm = normalize_program(program);
        if norm.is_empty() {
            return Err(Error::PermissionDenied("命令为空".into()));
        }
        if self.command_allowlist.iter().any(|c| c == &norm) {
            Ok(())
        } else {
            Err(Error::PermissionDenied(format!(
                "命令 `{program}` 不在白名单内，可在设置中显式添加（当前 {} 项）",
                self.command_allowlist.len()
            )))
        }
    }

    pub async fn authorize(&self, session_id: &str, tool: &str, args: &serde_json::Value, risk: RiskLevel, reason: &str) -> Result<()> {
        let needs_approval = match self.mode {
            ApprovalMode::ReadOnly => {
                if risk != RiskLevel::Low {
                    return Err(Error::PermissionDenied(format!("只读模式下禁止调用 `{tool}`")));
                }
                false
            }
            ApprovalMode::Ask => risk != RiskLevel::Low,
            ApprovalMode::Auto => risk == RiskLevel::High,
        };
        if !needs_approval {
            return Ok(());
        }
        let grant_key = format!("{session_id}:{tool}");
        if self.session_grants.lock().await.contains(&grant_key) {
            return Ok(());
        }
        let req = ApprovalRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            tool: tool.to_string(),
            args: args.clone(),
            risk,
            reason: reason.to_string(),
        };
        match self.approver.approve(req).await {
            Decision::AllowOnce => Ok(()),
            Decision::AllowSession => {
                self.session_grants.lock().await.insert(grant_key);
                Ok(())
            }
            Decision::Deny => Err(Error::PermissionDenied(format!("用户拒绝执行 `{tool}`"))),
        }
    }

    /// 切换会话时清空"本会话放行"
    pub async fn clear_session(&self) {
        self.session_grants.lock().await.clear();
    }
}

fn normalize_program(program: &str) -> String {
    let file = Path::new(program.trim())
        .file_name()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut out = file.as_str();
    for suffix in [".exe", ".cmd", ".bat", ".com"] {
        if let Some(stripped) = out.strip_suffix(suffix) {
            out = stripped;
            break;
        }
    }
    out.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn gate(root: &Path) -> PermissionGate {
        PermissionGate::new(ApprovalMode::Ask, root.to_path_buf(), vec![root.to_path_buf()], vec!["git".into(), "node".into()], Arc::new(DenyAll))
    }

    #[test]
    fn rejects_path_traversal() {
        let dir = tempdir().unwrap();
        let g = gate(dir.path());
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/a.txt"), "x").unwrap();
        assert!(g.jail("sub/a.txt").is_ok());
        assert!(g.jail("../escape.txt").is_err());
        assert!(g.jail("sub/../../etc/passwd").is_err());
        assert!(g.jail("C:/Windows/win.ini").is_err());
    }

    #[test]
    fn allows_new_file_under_root() {
        let dir = tempdir().unwrap();
        let g = gate(dir.path());
        assert!(g.jail("new_dir/output.md").is_ok());
    }

    #[test]
    fn command_allowlist_normalizes_suffix() {
        let dir = tempdir().unwrap();
        let g = gate(dir.path());
        assert!(g.check_command("git").is_ok());
        assert!(g.check_command("C:\\Program Files\\Git\\bin\\git.exe").is_ok());
        assert!(g.check_command("powershell").is_err());
        assert!(g.check_command("rm").is_err());
    }

    #[tokio::test]
    async fn ask_mode_denies_high_risk_without_approver() {
        let dir = tempdir().unwrap();
        let g = gate(dir.path());
        let err = g.authorize("s1", "run_command", &serde_json::json!({}), RiskLevel::High, "执行命令").await;
        assert!(matches!(err, Err(Error::PermissionDenied(_))));
    }
}
