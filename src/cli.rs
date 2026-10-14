use crate::model::{self, Compiler, Settings};
use anyhow::Result;
use clap::Args;
use std::{fs, path::PathBuf};

#[derive(Args)]
pub struct ModelOptions {
    /// Use a local GGUF instead of the pinned default model.
    #[arg(long, global = true)]
    model: Option<PathBuf>,
    #[arg(long, global = true)]
    cache_dir: Option<PathBuf>,
    /// Never download; fail if the default model is absent from cache.
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
}

impl ModelOptions {
    pub fn load(&self) -> Result<Compiler> {
        let (path, sha) = self.resolve()?;
        let prompt = match &self.prompt {
            Some(path) => fs::read_to_string(path)?,
            None => model::DEFAULT_PROMPT.into(),
        };
        eprintln!("Loading {} (in-process)…", path.display());
        Compiler::load(
            &path,
            sha,
            Settings {
                context: self.context,
                max_tokens: self.max_tokens,
                threads: self.threads,
                gpu_layers: self.gpu_layers,
            },
            prompt,
        )
    }

    pub fn resolve(&self) -> Result<(PathBuf, String)> {
        let cache = self.cache_dir.clone().unwrap_or_else(model::default_cache);
        model::resolve(self.model.as_deref(), &cache, self.offline)
    }
}
