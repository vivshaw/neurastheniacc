use crate::{
    model::{self, Compiler, Generation, Settings},
    source,
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{fs, path::Path, time::Instant};

#[derive(Serialize)]
pub struct Report {
    schema_version: u32,
    compiler_version: &'static str,
    platform: String,
    model_sha256: String,
    prompt_sha256: String,
    source_sha256: String,
    source: String,
    export: String,
    settings: Settings,
    model_load_seconds: f64,
    compilation_seconds: f64,
    pub status: String,
    error: Option<String>,
    generation: Option<Generation>,
    wasm_sha256: Option<String>,
}

pub fn compile_one(compiler: &Compiler, source: &source::Source, output: &Path) -> Result<Report> {
    ensure!(
        output.extension().is_some_and(|ext| ext == "wasm"),
        "output must have a .wasm extension"
    );
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    // A failed recompile must never leave a stale successful binary in place.
    for path in [
        output.to_path_buf(),
        output.with_extension("wat"),
        output.with_extension("raw.txt"),
    ] {
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    fs::write(output.with_extension("prompt.txt"), &compiler.prompt)?;
    let mut report = Report {
        schema_version: 2,
        compiler_version: env!("CARGO_PKG_VERSION"),
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        model_sha256: compiler.model_sha256.clone(),
        prompt_sha256: model::digest(compiler.prompt.as_bytes()),
        source_sha256: model::digest(source.canonical.as_bytes()),
        source: source.canonical.clone(),
        export: source.name.clone(),
        settings: compiler.settings.clone(),
        model_load_seconds: compiler.load_seconds,
        compilation_seconds: 0.0,
        status: "generation_error".into(),
        error: None,
        generation: None,
        wasm_sha256: None,
    };
    let start = Instant::now();
    let result = (|| -> Result<()> {
        let generation = compiler.generate(&source.canonical)?;
        fs::write(output.with_extension("raw.txt"), &generation.raw)?;
        report.generation = Some(generation);
        let generation = report.generation.as_ref().unwrap();
        report.status = "token_limit".into();
        ensure!(
            generation.stop == "eos",
            "model exhausted its output token budget"
        );
        report.status = "invalid_output".into();
        let wat = model::extract_wat(&generation.raw)?;
        fs::write(output.with_extension("wat"), &wat)?;
        let bytes = wat::parse_str(&wat).context("assembling generated WAT")?;
        report.status = "invalid_wasm".into();
        crate::wasm::Wasm::new(&bytes, &source.name)?;
        report.wasm_sha256 = Some(model::digest(&bytes));
        fs::write(output, &bytes)?;
        report.compilation_seconds = start.elapsed().as_secs_f64();
        report.status = "compiled".into();
        Ok(())
    })();
    if let Err(error) = result {
        report.error = Some(format!("{error:#}"));
        if report.compilation_seconds == 0.0 {
            report.compilation_seconds = start.elapsed().as_secs_f64();
        }
    }
    fs::write(
        output.with_extension("report.json"),
        serde_json::to_string_pretty(&report)?,
    )?;
    eprintln!(
        "{}: {} ({:.2}s)",
        source.name, report.status, report.compilation_seconds
    );
    if let Some(error) = &report.error {
        eprintln!("  {error}");
    }
    Ok(report)
}
