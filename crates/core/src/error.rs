use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("配置错误: {0}")]
    Config(String),

    #[error("网络错误: {0}")]
    Http(String),

    #[error("API 返回错误 (HTTP {status}): {body}")]
    Api { status: u16, body: String },

    #[error("响应解析失败: {0}")]
    Parse(String),

    #[error("音频错误: {0}")]
    Audio(String),

    #[error("IO 错误: {0}")]
    Io(String),

    #[error("缺少权限: {0}")]
    Permission(String),

    #[error("找不到可用的麦克风设备")]
    NoInputDevice,

    #[error("{0}")]
    Other(String),
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

impl Error {
    /// 这个错误**重试一下**是不是就可能好？
    ///
    /// - `Http`（网络/连接被拒/超时）→ 可能
    /// - `Api` 里的 429（限流）、5xx（服务端临时故障）、408（超时）→ 可能
    /// - 4xx 的其它状态（401 没授权、400 参数错）→ **重试没用**
    /// - 解析失败、配置错误等 → 重试没用
    pub fn is_transient(&self) -> bool {
        match self {
            Error::Http(_) => true,
            Error::Api { status, .. } => {
                *status == 429 || *status == 408 || (500..600).contains(status)
            }
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
