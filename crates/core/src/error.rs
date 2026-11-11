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

pub type Result<T> = std::result::Result<T, Error>;
