//! 语音处理流水线：prompt 渲染、词典替换、三模式编排。

pub mod dictionary;
pub mod template;
pub mod tools;
pub mod voice;

pub use dictionary::{apply_dictionary, DictionaryEntry};
pub use template::{render, PromptContext};
pub use tools::{ToolPerm, SAFE_COMMANDS};
pub use voice::{PipelineOutcome, VoicePipeline};
