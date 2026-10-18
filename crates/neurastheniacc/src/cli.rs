use crate::model::{self, ChatMode, Generator, Settings};
use anyhow::Result;
use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub struct ModelOptions {
    /// Select qwen-low (4B), qwen-high (27B), or a local GGUF path.
    #[arg(long, global = true, default_value = "qwen-low")]
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
    /// Set Qwen thinking (requires a compatible template). Bare flag enables it;
    /// omitted uses non-thinking for presets or the custom model's template.
    #[arg(long, global = true, num_args = 0..=1, require_equals = true, default_missing_value = "true")]
    qwen_thinking: Option<bool>,
}

impl ModelOptions {
    pub fn selection(&self) -> &str {
        &self.model
    }

    pub fn load(&self) -> Result<Generator> {
        let settings = Settings {
            context: self.context,
            max_tokens: self.max_tokens,
            threads: self.threads,
            gpu_layers: self.gpu_layers,
            chat_mode: self.chat_mode(),
        };
        // Invalid settings must fail before resolution downloads or hashes a model.
        settings.validate()?;
        let (path, sha) = self.resolve()?;
        eprintln!("Loading {} (in-process)…", path.display());
        Generator::load(&path, sha, settings)
    }

    fn chat_mode(&self) -> ChatMode {
        match self.qwen_thinking {
            Some(true) => ChatMode::QwenThinking,
            Some(false) => ChatMode::QwenNoThinking,
            None => model::preset(&self.model)
                .map_or(ChatMode::ModelTemplate, |preset| preset.chat_mode),
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
        assert_eq!(Cli::parse_from(["test"]).model.selection(), "qwen-low");
        for (args, expected) in [
            (vec!["test"], ChatMode::QwenNoThinking),
            (
                vec!["test", "--model", "qwen-low"],
                ChatMode::QwenNoThinking,
            ),
            (
                vec!["test", "--model", "qwen-high"],
                ChatMode::QwenNoThinking,
            ),
            (vec!["test", "--qwen-thinking"], ChatMode::QwenThinking),
            (vec!["test", "--qwen-thinking=true"], ChatMode::QwenThinking),
            (
                vec!["test", "--qwen-thinking=false"],
                ChatMode::QwenNoThinking,
            ),
            (
                vec!["test", "--model", "custom.gguf"],
                ChatMode::ModelTemplate,
            ),
            (
                vec!["test", "--model", "custom.gguf", "--qwen-thinking=false"],
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
        assert!(Cli::try_parse_from(["test", "--qwen-thinking=maybe"]).is_err());
        assert!(Cli::try_parse_from(["test", "--qwen-no-thinking"]).is_err());
    }

    #[test]
    fn invalid_settings_fail_before_model_resolution() {
        for (args, message) in [
            (vec!["test", "--context", "100"], "context"),
            (vec!["test", "--threads", "0"], "threads"),
            (
                vec!["test", "--context", "512", "--max-tokens", "512"],
                "max-tokens",
            ),
        ] {
            let result = Cli::try_parse_from(args).unwrap().model.load();
            let error = match result {
                Err(error) => error,
                Ok(_) => panic!("invalid settings unexpectedly loaded a model"),
            };
            assert!(
                format!("{error:#}").contains(message),
                "{error:#} should mention {message}"
            );
        }
    }
}
