use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("配置错误: {0}")]
    Config(String),

    #[error("HTTP 传输失败: {0}")]
    Http(String),

    #[error("鉴权失败: {0}")]
    Auth(String),

    #[error("上游 {status} 错误: {body}")]
    Upstream { status: u16, body: String, retryable: bool },

    #[error("触发限流，建议 {retry_after_ms}ms 重试")]
    RateLimited { retry_after_ms: u64 },

    #[error("响应解析失败: {0}")]
    Parse(String),

    #[error("工具执行失败: {0}")]
    Tool(String),

    #[error("权限被拒绝: {0}")]
    PermissionDenied(String),

    #[error("MCP 协议错误: {0}")]
    Mcp(String),

    #[error("存储错误: {0}")]
    Store(String),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON 错误: {0}")]
    Json(#[from] serde_json::Error),

    #[error("任务已取消")]
    Cancelled,

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// 是否值得自动重试（网络抖动 / 5xx / 限流）
    pub fn retryable(&self) -> bool {
        match self {
            Error::Http(_) | Error::RateLimited { .. } => true,
            Error::Upstream { retryable, .. } => *retryable,
            _ => false,
        }
    }

    /// 给前端看的稳定错误码，避免 UI 依赖中文文案
    pub fn code(&self) -> &'static str {
        match self {
            Error::Config(_) => "config",
            Error::Http(_) => "network",
            Error::Auth(_) => "auth",
            Error::Upstream { status, .. } if *status == 429 => "rate_limited",
            Error::Upstream { .. } => "upstream",
            Error::RateLimited { .. } => "rate_limited",
            Error::Parse(_) => "parse",
            Error::Tool(_) => "tool",
            Error::PermissionDenied(_) => "permission_denied",
            Error::Mcp(_) => "mcp",
            Error::Store(_) => "store",
            Error::Io(_) => "io",
            Error::Json(_) => "json",
            Error::Cancelled => "cancelled",
            Error::Other(_) => "other",
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Store(e.to_string())
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            return Error::Http(format!("请求超时: {e}"));
        }
        match e.status() {
            Some(s) if s == reqwest::StatusCode::UNAUTHORIZED || s == reqwest::StatusCode::FORBIDDEN => {
                Error::Auth(e.to_string())
            }
            Some(s) => Error::Upstream {
                status: s.as_u16(),
                body: e.to_string(),
                retryable: s.is_server_error() || s.as_u16() == 429,
            },
            None => Error::Http(e.to_string()),
        }
    }
}

/// 给前端序列化的错误信封
#[derive(Debug, Serialize)]
pub struct ErrorEnvelope {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl From<Error> for ErrorEnvelope {
    fn from(e: Error) -> Self {
        ErrorEnvelope { code: e.code().to_string(), message: e.to_string(), retryable: e.retryable() }
    }
}
