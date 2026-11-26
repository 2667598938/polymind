//! Tauri 侧的应用状态：一份配置、一份数据库、一份模型注册表。
//!
//! 约定：所有"可变更"的状态都用 `tokio::sync::RwLock`，因为命令是 async 的；
//! 每次发送消息都重新构建 Runtime，避免长时间持有的旧注册表与旧权限策略。

use async_trait::async_trait;
use polymind_core::agent::permission::{ApprovalRequest, Approver, Decision, PermissionGate};
use polymind_core::agent::runtime::{default_system_prompt, AgentConfig, AgentRuntime};
use polymind_core::agent::tools::ToolRegistry;
use polymind_core::config::AppConfig;
use polymind_core::mcp::McpManager;
use polymind_core::provider::Registry;
use polymind_core::secrets::SecretBox;
use polymind_core::store::Store;
use polymind_core::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::{oneshot, Mutex, RwLock};
use tokio_util::sync::CancellationToken;

pub type PendingApprovals = Arc<Mutex<HashMap<String, oneshot::Sender<Decision>>>>;

pub const EVENT_STREAM: &str = "polymind://event";
pub const EVENT_APPROVAL: &str = "polymind://approval";

pub struct AppState {
    pub home: PathBuf,
    pub workspace: RwLock<PathBuf>,
    pub config_path: PathBuf,
    pub config: RwLock<AppConfig>,
    pub secrets: SecretBox,
    pub store: Store,
    pub registry: RwLock<Arc<Registry>>,
    pub mcp: Mutex<McpManager>,
    pub cancel: Mutex<Option<CancellationToken>>,
    pub pending: PendingApprovals,
}

impl AppState {
    pub fn new(home: PathBuf) -> Result<Arc<Self>> {
        std::fs::create_dir_all(&home)?;
        let config_path = home.join("config.json");
        let mut config = AppConfig::load_or_init(&config_path)?;
        let secrets = SecretBox::open(&home)?;
        for p in &mut config.providers {
            p.api_key = secrets.open_sealed(&p.api_key_cipher).unwrap_or_default();
        }

        let workspace = config
            .allowed_roots
            .first()
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("workspace"));
        if !workspace.exists() {
            std::fs::create_dir_all(&workspace).ok();
        }
        if config.allowed_roots.is_empty() {
            config.allowed_roots = vec![workspace.to_string_lossy().to_string()];
            config.save(&config_path).ok();
        }
        // 首次启动给一个空工作区，避免用户第一句话就被"未授权目录"挡住
        let _ = std::fs::create_dir_all(&workspace);

        let store = Store::open(&home.join("polymind.db"))?;
        let registry = Registry::from_configs(&config.providers).unwrap_or_else(|_| Registry::new());

        Ok(Arc::new(AppState {
            home,
            workspace: RwLock::new(workspace),
            config_path,
            config: RwLock::new(config),
            secrets,
            store,
            registry: RwLock::new(Arc::new(registry)),
            mcp: Mutex::new(McpManager::default()),
            cancel: Mutex::new(None),
            pending: Arc::new(Mutex::new(HashMap::new())),
        }))
    }

    pub async fn registry(&self) -> Arc<Registry> {
        self.registry.read().await.clone()
    }

    pub async fn rebuild_registry(&self) -> Result<()> {
        let cfg = self.config.read().await.clone();
        let reg = match Registry::from_configs(&cfg.providers) {
            Ok(r) => r,
            Err(_) if cfg.providers.iter().all(|p| !p.enabled) => Registry::new(),
            Err(e) => return Err(e),
        };
        *self.registry.write().await = Arc::new(reg);
        Ok(())
    }

    /// 工具体 = 内置工具 + 所有 MCP 工具（首次调用时懒启动 MCP 子进程）
    pub async fn toolset(&self) -> Arc<ToolRegistry> {
        let mut mcp = self.mcp.lock().await;
        if mcp.is_empty() {
            let cfgs = self.config.read().await.clone().mcp_servers;
            *mcp = McpManager::start(&cfgs).await;
        }
        let mut tools = ToolRegistry::builtin();
        mcp.register_into(&mut tools).await;
        Arc::new(tools)
    }

    pub async fn gate(&self, app: AppHandle) -> Arc<PermissionGate> {
        let cfg = self.config.read().await.clone();
        let workspace = self.workspace.read().await.clone();
        Arc::new(PermissionGate::from_config(
            &cfg,
            &workspace,
            Arc::new(UiApprover { app, pending: self.pending.clone() }),
        ))
    }

    pub async fn runtime(&self, app: AppHandle) -> AgentRuntime {
        let cfg = self.config.read().await.clone();
        let workspace = self.workspace.read().await.clone();
        AgentRuntime {
            registry: self.registry().await,
            tools: self.toolset().await,
            gate: self.gate(app).await,
            config: AgentConfig {
                system_prompt: default_system_prompt(&workspace, &cfg.system_prompt_extra),
                max_steps: cfg.max_steps,
                context_budget_tokens: cfg.context_budget_tokens,
                temperature: None,
                max_tokens: Some(8192),
                fallback_models: cfg.fallback_models.clone(),
                tool_timeout: Duration::from_secs(180),
            },
            workspace,
        }
    }
}

/// 把内核的审批请求转发到前端，等用户点按钮。
/// 600s 无响应按"拒绝"处理，绝不默认放行。
pub struct UiApprover {
    pub app: AppHandle,
    pub pending: PendingApprovals,
}

#[async_trait]
impl Approver for UiApprover {
    async fn approve(&self, req: ApprovalRequest) -> Decision {
        let request_id = req.request_id.clone();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(request_id.clone(), tx);
        if self.app.emit(EVENT_APPROVAL, &req).is_err() {
            self.pending.lock().await.remove(&request_id);
            return Decision::Deny;
        }
        match tokio::time::timeout(Duration::from_secs(600), rx).await {
            Ok(Ok(decision)) => decision,
            _ => {
                self.pending.lock().await.remove(&request_id);
                Decision::Deny
            }
        }
    }
}
