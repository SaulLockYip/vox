//! 模型服务接入层：MiniMax ASR + Anthropic 兼容 Messages。

pub mod asr;
pub mod llm;
pub mod sse;

pub use asr::{AsrClient, AsrResult, Segment};
pub use llm::{LlmClient, LlmRequest};
