//! Vox 核心：配置模型、错误类型、模式定义。
//! 不依赖任何平台 API，可被 CLI / Tauri / 测试共同使用。

pub mod config;
pub mod error;
pub mod instance;
pub mod mode;
pub mod secrets;

pub use config::{
    Config, GeneralConfig, InsertMethod, LlmConfig, PromptsConfig, SttConfig, ThinkingMode,
};
pub use error::{Error, Result};
pub use mode::{Mode, MODE_LABELS};
