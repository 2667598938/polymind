//! 内置工具集。每个工具都必须自己声明风险等级与"人话理由"，
//! 审批弹窗直接展示这句理由，而不是让用户去猜 JSON。

use async_trait::async_trait;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::permission::{PermissionGate, RiskLevel};
use crate::model::{ToolSource, ToolSpec};
use crate::Result;

pub mod fs;
pub mod shell;
pub mod web;

#[derive(Clone)]
pub struct ToolCtx {
    pub workspace: PathBuf,
    pub gate: Arc<PermissionGate>,
    pub session_id: String,
}

#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>) -> Self {
        ToolOutput { content: content.into(), is_error: false }
    }
    pub fn err(content: impl Into<String>) -> Self {
        ToolOutput { content: content.into(), is_error: true }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    fn risk(&self, args: &Value) -> RiskLevel;
    fn reason(&self, args: &Value) -> String;
    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput>;
}

#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn builtin() -> Self {
        let mut r = ToolRegistry::default();
        r.register(Arc::new(fs::ReadFile));
        r.register(Arc::new(fs::WriteFile));
        r.register(Arc::new(fs::ListDir));
        r.register(Arc::new(fs::Grep));
        r.register(Arc::new(shell::RunCommand));
        r.register(Arc::new(web::WebFetch));
        r
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let spec = tool.spec();
        self.tools.insert(spec.name.clone(), tool);
    }

    pub fn unregister(&mut self, name: &str) {
        self.tools.remove(name);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|t| t.spec()).collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

pub fn builtin_spec(name: &str) -> Option<ToolSpec> {
    ToolRegistry::builtin().get(name).map(|t| t.spec())
}

#[allow(dead_code)]
fn marker() -> ToolSource {
    ToolSource::Builtin
}
