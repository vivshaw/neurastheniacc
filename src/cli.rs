use crate::model::{self, ChatMode, Compiler, Settings};
use anyhow::Result;
use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub struct ModelOptions {
    /// Select high (27B), low (4B), or a local GGUF path.
    #[arg(long, global = true, default_value = "high")]
    model: String,
    #[arg(long, global = true)]
    cache_dir: Option<PathBuf>,
    /// Never download; fail if the selected model is absent from cache.
    #[arg(long, global = true)]
    offline: bool,
    #[arg(long, global = true, default_value_t = 8192)]
    context: u32,
    #[arg(long, global = true, default_value_t = 1536)]
    max_tokens: u32,
    #[arg(long, global = true, default_value_t = 4)]
    threads: i32,
    /// Use 0 for CPU-only inference. Metal is bundled on macOS.
    #[arg(long, global = true, default_value_t = 99)]
    gpu_layers: u32,
    /// Use Qwen's explicit non-thinking chat format (requires a compatible template).
    #[arg(long, global = true)]
    qwen_no_thinking: bool,
    /// Enable Qwen thinking; overrides the presets' non-thinking default.
    #[arg(long, global = true, conflicts_with = "qwen_no_thinking")]
    qwen_thinking: bool,
}

impl ModelOptions {
    pub fn load(&self) -> Result<Compiler> {
        self.load_with_prompt(model::DEFAULT_PROMPT.into())
    }

    pub fn load_with_prompt(&self, prompt: String) -> Result<Compiler> {
        let (path, sha) = self.resolve()?;
        eprintln!("Loading {} (in-process)…", path.display());
        Compiler::load(
            &path,
            sha,
            Settings {
                context: self.context,
                max_tokens: self.max_tokens,
                threads: self.threads,
                gpu_layers: self.gpu_layers,
                chat_mode: self.chat_mode(),
            },
            prompt,
        )
    }

    fn chat_mode(&self) -> ChatMode {
        if self.qwen_thinking {
            ChatMode::QwenThinking
        } else if self.qwen_no_thinking {
            ChatMode::QwenNoThinking
        } else {
            model::preset(&self.model).map_or(ChatMode::ModelTemplate, |preset| preset.chat_mode)
        }
    }

    pub fn resolve(&self) -> Result<(PathBuf, String)> {
        let cache = self.cache_dir.clone().unwrap_or_else(model::default_cache);
        model::resolve(&self.model, &cache, self.offline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        model: ModelOptions,
    }

    #[test]
    fn resolves_presets_custom_templates_and_explicit_overrides() {
        for (args, expected) in [
            (vec!["test"], ChatMode::QwenNoThinking),
            (vec!["test", "--model", "low"], ChatMode::QwenNoThinking),
            (vec!["test", "--qwen-thinking"], ChatMode::QwenThinking),
            (
                vec!["test", "--model", "custom.gguf"],
                ChatMode::ModelTemplate,
            ),
            (
                vec!["test", "--model", "custom.gguf", "--qwen-no-thinking"],
                ChatMode::QwenNoThinking,
            ),
            (
                vec!["test", "--model", "custom.gguf", "--qwen-thinking"],
                ChatMode::QwenThinking,
            ),
        ] {
            assert_eq!(
                Cli::try_parse_from(args).unwrap().model.chat_mode(),
                expected
            );
        }
        assert!(Cli::try_parse_from(["test", "--qwen-thinking", "--qwen-no-thinking"]).is_err());
    }
}
