use crate::model::{self, Compiler, Settings};
use anyhow::Result;
use clap::Args;
use std::{fs, path::PathBuf};

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
    /// Override the versioned compiler system prompt.
    #[arg(long, global = true)]
    prompt: Option<PathBuf>,
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
        let (path, sha) = self.resolve()?;
        let prompt = match &self.prompt {
            Some(path) => fs::read_to_string(path)?,
            None => model::DEFAULT_PROMPT.into(),
        };
        eprintln!("Loading {} (in-process)…", path.display());
        let compiler = Compiler::load(
            &path,
            sha,
            Settings {
                context: self.context,
                max_tokens: self.max_tokens,
                threads: self.threads,
                gpu_layers: self.gpu_layers,
                qwen_no_thinking: !self.qwen_thinking
                    && (self.qwen_no_thinking
                        || model::preset(&self.model)
                            .is_some_and(|preset| preset.qwen_no_thinking)),
                qwen_thinking: self.qwen_thinking,
            },
            prompt,
        )?;
        Ok(compiler)
    }

    pub fn resolve(&self) -> Result<(PathBuf, String)> {
        let cache = self.cache_dir.clone().unwrap_or_else(model::default_cache);
        // Reuse verified weights downloaded before the compiler was renamed.
        if self.cache_dir.is_none()
            && let Some(preset) = model::preset(&self.model)
            && !cache.join(preset.file).exists()
        {
            let legacy_cache = cache.with_file_name("neuralcc");
            if legacy_cache.join(preset.file).exists() {
                return model::resolve(&self.model, &legacy_cache, self.offline);
            }
        }
        model::resolve(&self.model, &cache, self.offline)
    }
}
