use crate::{
    model::{self, Compiler, Generation, Settings},
    source,
    wasm::Wasm,
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

pub const DEFAULT_ATTEMPTS: u32 = 5;

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
    assembler: Assembler,
    assembly: Option<crate::assembly::Report>,
    max_attempts: u32,
    attempt_directory: PathBuf,
    attempts: Vec<Attempt>,
}

#[derive(Serialize)]
struct Attempt {
    number: u32,
    input: String,
    status: String,
    error: Option<String>,
    generation: Option<Generation>,
    seconds: f64,
}

#[derive(Clone, Copy, Serialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Assembler {
    Wat,
    Model,
}

pub fn compile_one(compiler: &Compiler, source: &source::Source, output: &Path) -> Result<Report> {
    compile_with_options(compiler, source, output, Assembler::Wat, DEFAULT_ATTEMPTS)
}

pub fn compile_with_options(
    compiler: &Compiler,
    source: &source::Source,
    output: &Path,
    assembler: Assembler,
    max_attempts: u32,
) -> Result<Report> {
    ensure!(
        (1..=5).contains(&max_attempts),
        "attempts must be between 1 and 5"
    );
    ensure!(
        output.extension().is_some_and(|ext| ext == "wasm"),
        "output must have a .wasm extension"
    );
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    // Preserve every invocation's history, including when the output name is reused.
    let attempt_directory = tempfile::Builder::new()
        .prefix(&format!(
            "{}.attempts-",
            output.file_stem().unwrap().to_string_lossy()
        ))
        .tempdir_in(parent)?
        .keep();
    for extension in [
        "wasm",
        "wat",
        "raw.txt",
        "candidate.wasm",
        "assembly.raw.txt",
        "assembly.input.wat",
        "assembly.prompt.txt",
        "assembly.report.json",
    ] {
        let path = output.with_extension(extension);
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    fs::write(output.with_extension("prompt.txt"), &compiler.prompt)?;
    fs::write(
        attempt_directory.join("system.prompt.txt"),
        &compiler.prompt,
    )?;
    let start = Instant::now();
    let (attempts, bytes) = wat_loop(source, max_attempts, &attempt_directory, |input| {
        compiler.generate(input)
    })?;
    let last = attempts.last().unwrap();
    let mut report = Report {
        schema_version: 4,
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
        status: last.status.clone(),
        error: last.error.clone(),
        generation: last.generation.clone(),
        wasm_sha256: None,
        assembler,
        assembly: None,
        max_attempts,
        attempt_directory,
        attempts,
    };
    if let Some(generation) = &report.generation {
        fs::write(output.with_extension("raw.txt"), &generation.raw)?;
    }
    let last_wat = report
        .attempt_directory
        .join(format!("attempt-{}.wat", report.attempts.len()));
    if last_wat.exists() {
        fs::copy(&last_wat, output.with_extension("wat"))?;
    }
    if let Some(mut bytes) = bytes {
        let result = (|| -> Result<()> {
            if matches!(assembler, Assembler::Model) {
                // Each stage has its own bounded validation-feedback loop.
                report.status = "assembly_error".into();
                report.assembly = Some(crate::assembly::assemble(
                    compiler,
                    &fs::read_to_string(&last_wat)?,
                    &source.name,
                    output,
                    max_attempts,
                )?);
                ensure!(
                    report.assembly.as_ref().unwrap().status == "assembled",
                    "model assembly failed; see assembly report"
                );
                bytes = fs::read(output)?;
            }
            report.wasm_sha256 = Some(model::digest(&bytes));
            fs::write(output, bytes)?;
            report.status = "compiled".into();
            Ok(())
        })();
        if let Err(error) = result {
            report.error = Some(format!("{error:#}"));
        }
    }
    report.compilation_seconds = start.elapsed().as_secs_f64();
    fs::write(
        output.with_extension("report.json"),
        serde_json::to_string_pretty(&report)?,
    )?;
    fs::write(
        report.attempt_directory.join("report.json"),
        serde_json::to_string_pretty(&report)?,
    )?;
    eprintln!(
        "{}: {} ({} attempts, {:.2}s)",
        source.name,
        report.status,
        report.attempts.len(),
        report.compilation_seconds
    );
    Ok(report)
}

fn feedback(source: &str, attempt: &Attempt) -> String {
    format!(
        "The previous translation failed validation. Correct the WAT while preserving the original C semantics. Return the complete replacement module only.\n\nOriginal C:\n{source}\n\nPrevious response:\n{}\n\nValidation result ({}):\n{}\n",
        attempt.generation.as_ref().map_or("", |g| g.raw.as_str()),
        attempt.status,
        attempt.error.as_deref().unwrap_or("unknown error")
    )
}

// Keeping generation injectable lets tests exercise the actual compiler loop without a model.
fn wat_loop(
    source: &source::Source,
    max_attempts: u32,
    directory: &Path,
    mut generate: impl FnMut(&str) -> Result<Generation>,
) -> Result<(Vec<Attempt>, Option<Vec<u8>>)> {
    ensure!(
        (1..=5).contains(&max_attempts),
        "attempts must be between 1 and 5"
    );
    let mut attempts = Vec::new();
    let mut input = source.canonical.clone();
    for number in 1..=max_attempts {
        let prefix = directory.join(format!("attempt-{number}"));
        fs::write(prefix.with_extension("input.txt"), &input)?;
        let start = Instant::now();
        let mut attempt = Attempt {
            number,
            input: input.clone(),
            status: "generation_error".into(),
            error: None,
            generation: None,
            seconds: 0.0,
        };
        let result = (|| -> Result<Vec<u8>> {
            attempt.generation = Some(generate(&input)?);
            let generation = attempt.generation.as_ref().unwrap();
            fs::write(prefix.with_extension("raw.txt"), &generation.raw)?;
            attempt.status = "token_limit".into();
            ensure!(
                generation.stop == "eos",
                "response reached its output token limit; return a complete, concise WAT module within the same budget"
            );
            attempt.status = "invalid_output".into();
            let wat = model::extract_wat(&generation.raw)?;
            fs::write(prefix.with_extension("wat"), &wat)?;
            let bytes = wat::parse_str(&wat).context("assembling generated WAT")?;
            attempt.status = "invalid_wasm".into();
            Wasm::new(&bytes, &source.name)?;
            fs::write(prefix.with_extension("wasm"), &bytes)?;
            attempt.status = "compiled".into();
            Ok(bytes)
        })();
        attempt.seconds = start.elapsed().as_secs_f64();
        if let Err(error) = &result {
            attempt.error = Some(format!("{error:#}"));
        }
        fs::write(
            prefix.with_extension("report.json"),
            serde_json::to_string_pretty(&attempt)?,
        )?;
        eprintln!(
            "{}: attempt {number}/{max_attempts}: {}",
            source.name, attempt.status
        );
        let retry = matches!(
            attempt.status.as_str(),
            "invalid_output" | "invalid_wasm" | "token_limit"
        );
        input = feedback(&source.canonical, &attempt);
        attempts.push(attempt);
        if let Ok(bytes) = result {
            return Ok((attempts, Some(bytes)));
        }
        if !retry {
            break;
        }
    }
    Ok((attempts, None))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn source() -> source::Source {
        source::parse("uint32_t f(uint32_t x, uint32_t y) { return x + y; }").unwrap()
    }
    fn generation(raw: &str) -> Generation {
        Generation {
            raw: raw.into(),
            chat_prompt_sha256: String::new(),
            prompt_tokens: 0,
            output_tokens: 0,
            seconds: 0.0,
            stop: "eos",
        }
    }
    const VALID: &str = "(module (func (export \"f\") (param i32 i32) (result i32) local.get 0 local.get 1 i32.add))";
    #[test]
    fn feeds_syntax_and_validation_errors_back_and_stops_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let src = source();
        let mut calls = 0;
        let (attempts, bytes) = wat_loop(&src, 5, dir.path(), |input| {
            calls += 1;
            match calls {
                1 => {
                    assert_eq!(input, src.canonical);
                    Ok(generation("(module"))
                }
                2 => {
                    assert!(input.contains("assembling generated WAT"));
                    assert!(input.contains(&src.canonical));
                    Ok(generation(
                        "(module (func (export \"f\") (param i32 i32) (result i32) local.get 9))",
                    ))
                }
                3 => {
                    assert!(input.contains("invalid_wasm"));
                    assert!(input.contains("local.get 9"));
                    Ok(generation(VALID))
                }
                _ => panic!("retried after success"),
            }
        })
        .unwrap();
        assert_eq!(attempts.len(), 3);
        assert!(bytes.is_some());
        assert!(dir.path().join("attempt-1.raw.txt").exists());
        assert!(dir.path().join("attempt-3.wasm").exists());
    }
    #[test]
    fn respects_attempt_limit_and_does_not_retry_infrastructure_errors() {
        let dir = tempfile::tempdir().unwrap();
        for limit in [1, 5] {
            let (attempts, bytes) =
                wat_loop(&source(), limit, dir.path(), |_| Ok(generation("bad"))).unwrap();
            assert_eq!(attempts.len(), limit as usize);
            assert!(bytes.is_none());
        }
        let (attempts, bytes) = wat_loop(&source(), 5, dir.path(), |_| {
            anyhow::bail!("inference unavailable")
        })
        .unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].status, "generation_error");
        assert!(bytes.is_none());
    }
    #[test]
    fn valid_but_wrong_program_stops_without_semantic_feedback() {
        let dir = tempfile::tempdir().unwrap();
        let (attempts, bytes) = wat_loop(&source(), 5, dir.path(), |_| {
            Ok(generation(
                "(module (func (export \"f\") (param i32 i32) (result i32) i32.const 0))",
            ))
        })
        .unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(
            Wasm::new(&bytes.unwrap(), "f").unwrap().run(3, 5).unwrap(),
            0
        );
    }
}
