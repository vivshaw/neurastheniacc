use crate::prompt::{qwen_prompt, split_thinking};
use anyhow::{Context, Result, ensure};
use llama_cpp_2::{
    LogOptions,
    context::params::LlamaContextParams,
    llama_backend::LlamaBackend,
    llama_batch::LlamaBatch,
    model::{LlamaChatMessage, LlamaModel, params::LlamaModelParams},
    sampling::LlamaSampler,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    num::NonZeroU32,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub struct ModelPreset {
    pub file: &'static str,
    pub sha: &'static str,
    pub size: u64,
    pub url: &'static str,
    pub chat_mode: ChatMode,
}

pub fn preset(name: &str) -> Option<ModelPreset> {
    match name {
        "low" => Some(ModelPreset {
            file: "Qwen3.5-4B-Q4_K_M.gguf",
            sha: "00fe7986ff5f6b463e62455821146049db6f9313603938a70800d1fb69ef11a4",
            size: 2_740_937_888,
            url: "https://huggingface.co/unsloth/Qwen3.5-4B-GGUF/resolve/e87f176479d0855a907a41277aca2f8ee7a09523/Qwen3.5-4B-Q4_K_M.gguf",
            chat_mode: ChatMode::QwenNoThinking,
        }),
        "high" => Some(ModelPreset {
            file: "Qwen3.8-27B-UD-Q4_K_M.gguf",
            sha: "322e194ff79741c7baa497c240f677f54b201b0efab44ca8e50f122b39123482",
            size: 16_464_440_224,
            url: "https://huggingface.co/unsloth/Qwen3.8-27B-GGUF/resolve/4ca720788d1e01f1bff70c033e0d0028fd02e502/Qwen3.8-27B-UD-Q4_K_M.gguf",
            chat_mode: ChatMode::QwenNoThinking,
        }),
        _ => None,
    }
}
pub const DEFAULT_PROMPT: &str = include_str!("../prompts/v4.txt");

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub fn file_digest(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buf = [0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hex(&hash.finalize()))
}

pub fn default_cache() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(dir).join("neurastheniacc");
    }
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into()));
    if cfg!(target_os = "macos") {
        home.join("Library/Caches/neurastheniacc")
    } else {
        home.join(".cache/neurastheniacc")
    }
}

pub fn resolve(selection: &str, cache: &Path, offline: bool) -> Result<(PathBuf, String)> {
    let Some(model) = preset(selection) else {
        let path = Path::new(selection);
        return Ok((
            path.to_path_buf(),
            file_digest(path).context("reading local model")?,
        ));
    };
    let path = cache.join(model.file);
    if path.exists() {
        ensure!(
            fs::metadata(&path)?.len() == model.size && file_digest(&path)? == model.sha,
            "cached model checksum mismatch; remove {} and retry",
            path.display()
        );
        return Ok((path, model.sha.to_string()));
    }
    ensure!(
        !offline,
        "model is not cached at {}; run `neurastheniacc --model {selection} fetch` online first",
        path.display()
    );
    fs::create_dir_all(cache)?;
    eprintln!(
        "Downloading {} ({:.2} GB) to {}",
        model.file,
        model.size as f64 / 1e9,
        path.display()
    );
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(3600))
        .build()?;
    let mut response = client.get(model.url).send()?.error_for_status()?;
    // Unique temporary file + atomic rename: interrupted/concurrent downloads cannot poison the cache.
    let mut temporary = tempfile::NamedTempFile::new_in(cache)?;
    let mut hash = Sha256::new();
    let mut buf = [0u8; 256 * 1024];
    let mut size = 0u64;
    let mut last_update = Instant::now();
    loop {
        let n = response.read(&mut buf)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        ensure!(size <= model.size, "download exceeds expected model size");
        hash.update(&buf[..n]);
        temporary.write_all(&buf[..n])?;
        if last_update.elapsed() >= Duration::from_secs(3) {
            eprintln!(
                "  {:.0}% ({:.0} MB)",
                size as f64 / model.size as f64 * 100.0,
                size as f64 / 1e6
            );
            last_update = Instant::now();
        }
    }
    ensure!(
        size == model.size && hex(&hash.finalize()) == model.sha,
        "download checksum/size mismatch"
    );
    temporary.as_file().sync_all()?;
    temporary.persist(&path)?;
    eprintln!("Model verified and cached.");
    Ok((path, model.sha.to_string()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatMode {
    ModelTemplate,
    QwenNoThinking,
    QwenThinking,
}

#[derive(Clone, Serialize)]
pub struct Settings {
    pub context: u32,
    pub max_tokens: u32,
    pub threads: i32,
    pub gpu_layers: u32,
    pub chat_mode: ChatMode,
}

#[derive(Clone, Serialize)]
pub struct Generation {
    pub raw: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    pub prompt_tokens: usize,
    pub output_tokens: usize,
    pub seconds: f64,
    pub stop: &'static str,
}

impl Generation {
    pub fn final_text(&self) -> Result<&str> {
        if self.reasoning.is_some() {
            self.answer
                .as_deref()
                .context("thinking response has no closing </think> delimiter")
        } else {
            Ok(&self.raw)
        }
    }
}

pub struct Compiler {
    model: LlamaModel,
    backend: LlamaBackend,
    pub settings: Settings,
    pub model_sha256: String,
    pub load_seconds: f64,
    pub prompt: String,
}

impl Compiler {
    pub fn load(
        path: &Path,
        model_sha256: String,
        settings: Settings,
        prompt: String,
    ) -> Result<Self> {
        ensure!(
            settings.context >= 512 && settings.context <= 32768,
            "context must be between 512 and 32768"
        );
        ensure!(
            settings.max_tokens > 0 && settings.max_tokens < settings.context,
            "max-tokens must be positive and smaller than context"
        );
        ensure!(settings.threads > 0, "threads must be positive");
        llama_cpp_2::send_logs_to_tracing(LogOptions::default().with_logs_enabled(false));
        let start = Instant::now();
        let backend = LlamaBackend::init()?;
        let mut params = LlamaModelParams::default().with_n_gpu_layers(settings.gpu_layers);
        if settings.gpu_layers == 0 {
            params = params.with_devices(&[])?;
        }
        let model =
            LlamaModel::load_from_file(&backend, path, &params).context("loading GGUF model")?;
        Ok(Self {
            model,
            backend,
            settings,
            model_sha256,
            load_seconds: start.elapsed().as_secs_f64(),
            prompt,
        })
    }

    pub fn generate(&self, source: &str) -> Result<Generation> {
        let system = &self.prompt;
        let start = Instant::now();
        let template = self
            .model
            .chat_template(None)
            .context("model needs a supported chat template")?;
        let messages = [
            LlamaChatMessage::new("system".into(), system.to_owned())?,
            LlamaChatMessage::new("user".into(), source.into())?,
        ];
        let prompt = if self.settings.chat_mode != ChatMode::ModelTemplate {
            qwen_prompt(
                template.to_str()?,
                system,
                source,
                self.settings.chat_mode == ChatMode::QwenThinking,
            )?
        } else {
            self.model.apply_chat_template(&template, &messages, true)?
        };
        let vocab = self.model.vocab();
        let tokens = vocab.tokenize(prompt.as_bytes(), true, true);
        ensure!(
            tokens.len() + self.settings.max_tokens as usize <= self.settings.context as usize,
            "prompt ({} tokens) plus output budget exceeds context ({})",
            tokens.len(),
            self.settings.context
        );
        let params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(self.settings.context))
            .with_n_batch(512)
            .with_offload_kqv(self.settings.gpu_layers > 0)
            .with_op_offload(self.settings.gpu_layers > 0)
            .with_n_threads(self.settings.threads)
            .with_n_threads_batch(self.settings.threads);
        let mut ctx = self.model.new_context(&self.backend, params).context(
            "creating inference context; if GPU access is unavailable, try --gpu-layers 0",
        )?;
        let mut batch = LlamaBatch::new(512, 1);
        for (chunk_idx, chunk) in tokens.chunks(512).enumerate() {
            batch.clear();
            for (j, &token) in chunk.iter().enumerate() {
                let pos = chunk_idx * 512 + j;
                batch.add(token, pos as i32, &[0], pos == tokens.len() - 1)?;
            }
            ctx.decode(&mut batch)?;
        }
        let mut sampler = LlamaSampler::greedy();
        let mut bytes = Vec::new();
        let mut count = 0;
        let mut stop = "token_limit";
        for index in 0..self.settings.max_tokens {
            let token = sampler.sample(&ctx, batch.n_tokens() - 1);
            sampler.accept(token);
            if vocab.is_eog(token) {
                stop = "eos";
                break;
            }
            // llama-cpp-2's second argument is `special`: retain </think> so
            // reasoning can be separated from the final answer. EOG is handled above.
            let include_special = self.settings.chat_mode == ChatMode::QwenThinking;
            bytes.extend(vocab.token_to_piece(token, include_special, None));
            count += 1;
            batch.clear();
            batch.add(token, (tokens.len() + index as usize) as i32, &[0], true)?;
            ctx.decode(&mut batch)?;
        }
        let raw = String::from_utf8(bytes).context("model emitted invalid UTF-8")?;
        let (reasoning, answer) = if self.settings.chat_mode == ChatMode::QwenThinking {
            split_thinking(&raw)
        } else {
            (None, None)
        };
        Ok(Generation {
            raw,
            reasoning,
            answer,
            prompt_tokens: tokens.len(),
            output_tokens: count,
            seconds: start.elapsed().as_secs_f64(),
            stop,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offline_missing_cache_is_an_error_without_creating_files() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing");
        assert!(resolve("high", &missing, true).is_err());
        assert!(!missing.exists());
    }
    #[test]
    fn corrupted_cache_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join(preset("high").unwrap().file), "bad").unwrap();
        assert!(resolve("high", temp.path(), true).is_err());
    }
}
