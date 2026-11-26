//! PolyMind 内核
//!
//! 分层：
//! - `model`    : 与厂商无关的统一消息/流式事件/工具描述
//! - `provider` : OpenAI 兼容、Anthropic、Gemini 三类适配器 + 注册表 + 路由回退
//! - `agent`    : ReAct 循环、工具注册表、权限门、上下文预算
//! - `mcp`      : MCP(JSON-RPC over stdio) 客户端与管理器
//! - `store`    : SQLite 会话/用量/配置持久化
//! - `secrets`  : 本地密钥库（ChaCha20-Poly1305）

pub mod agent;
pub mod config;
pub mod error;
pub mod mcp;
pub mod model;
pub mod provider;
pub mod secrets;
pub mod store;

pub use error::{Error, Result};
