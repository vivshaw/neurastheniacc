use crate::{
    artifact,
    model::{self, Generation, Generator, Settings, StopReason},
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

pub const MAX_ATTEMPTS: u32 = 5;
pub const DEFAULT_ATTEMPTS: u32 = MAX_ATTEMPTS;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Compiled,
    GenerationError,
    TokenLimit,
    InvalidOutput,
    InvalidWat,
    InvalidWasm,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compiled => "compiled",
            Self::GenerationError => "generation_error",
            Self::TokenLimit => "token_limit",
            Self::InvalidOutput => "invalid_output",
            Self::InvalidWat => "invalid_wat",
            Self::InvalidWasm => "invalid_wasm",
        }
    }

    fn retryable(self) -> bool {
        match self {
            Self::InvalidOutput | Self::InvalidWat | Self::InvalidWasm | Self::TokenLimit => true,
            Self::Compiled | Self::GenerationError => false,
        }
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Serialize)]
pub struct Report {
    compiler_version: &'static str,
    platform: String,
    model_sha256: String,
    source_sha256: String,
    export: String,
    settings: Settings,
    model_load_seconds: f64,
    compilation_seconds: f64,
    pub status: Status,
    pub error: Option<String>,
    wasm_sha256: Option<String>,
    max_attempts: u32,
    attempt_count: usize,
}

impl Report {
    pub fn attempt_count(&self) -> usize {
        self.attempt_count
    }

    pub fn compilation_seconds(&self) -> f64 {
        self.compilation_seconds
    }
}

#[derive(Serialize)]
pub struct Attempt {
    pub number: u32,
    pub status: Status,
    pub error: Option<String>,
    pub generation: Option<Generation>,
    pub seconds: f64,
}

/// Every path derived from the output; construction lives here so callers cannot disagree.
pub struct Artifacts {
    pub wasm: PathBuf,
    pub wat: PathBuf,
}

impl Artifacts {
    pub fn new(output: &Path) -> Self {
        Self {
            wasm: output.with_extension("wasm"),
            wat: output.with_extension("wat"),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Path> {
        [&self.wasm, &self.wat].into_iter().map(PathBuf::as_path)
    }
}

/// Resolve the deepest existing ancestor so paths that do not exist yet still compare by identity.
fn resolve(path: &Path) -> PathBuf {
    if let Ok(resolved) = fs::canonicalize(path) {
        return resolved;
    }
    match path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(parent) => resolve(parent).join(path.file_name().unwrap_or_default()),
        None => path.to_path_buf(),
    }
}

/// Neither compilation output may overwrite the source itself.
pub fn ensure_artifacts_do_not_clobber(source: &Path, output: &Path) -> Result<()> {
    let source = resolve(source);
    for artifact in Artifacts::new(output).iter() {
        ensure!(
            resolve(artifact) != source,
            "compiling to {} would overwrite the source {}",
            artifact.display(),
            source.display()
        );
    }
    Ok(())
}

/// A compilation report paired with the validated module it describes, so evaluation
/// cannot pair a report with unrelated bytes read back from disk.
pub struct Compilation {
    pub report: Report,
    /// Validated module bytes; present only when `report.status` is `Compiled`.
    pub output: Option<Vec<u8>>,
}

/// Called after each completed attempt with its input and extracted WAT, when available.
/// An observer error aborts compilation; it is never treated as a model failure or retried.
/// Borrowed data is valid only for the call. Observers cannot change retry decisions.
pub type AttemptObserver<'a> = dyn FnMut(&Attempt, &str, Option<&str>) -> Result<()> + 'a;

#[derive(Default)]
pub struct Options<'a> {
    pub verbose: bool,
    pub on_attempt: Option<&'a mut AttemptObserver<'a>>,
}

pub fn compile(
    compiler: &Generator,
    source: &source::Source,
    output: &Path,
    max_attempts: u32,
    options: Options<'_>,
) -> Result<Compilation> {
    ensure!(
        (1..=MAX_ATTEMPTS).contains(&max_attempts),
        "attempts must be between 1 and {MAX_ATTEMPTS}"
    );
    ensure!(
        output.extension().is_some_and(|ext| ext == "wasm"),
        "output must have a .wasm extension"
    );
    let artifacts = Artifacts::new(output);
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    // A failed recompile must not leave an older binary looking like its result.
    for artifact in artifacts.iter() {
        if artifact.exists() {
            fs::remove_file(artifact)?;
        }
    }
    let start = Instant::now();
    let LoopOutput {
        attempts,
        bytes,
        wat,
    } = wat_loop(
        source,
        max_attempts,
        options.on_attempt,
        options.verbose,
        |input| compiler.generate(input),
    )?;
    let last = attempts.last().unwrap();
    let mut report = Report {
        compiler_version: env!("CARGO_PKG_VERSION"),
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        model_sha256: compiler.model_sha256().to_owned(),
        source_sha256: model::digest(source.canonical().as_bytes()),
        export: source.name().to_owned(),
        settings: compiler.settings().clone(),
        model_load_seconds: compiler.load_seconds(),
        compilation_seconds: 0.0,
        status: last.status,
        error: last.error.clone(),
        wasm_sha256: None,
        max_attempts,
        attempt_count: attempts.len(),
    };
    if let Some(wat) = wat {
        artifact::write(&artifacts.wat, wat)?;
    }
    if let Some(bytes) = &bytes {
        artifact::write(&artifacts.wasm, bytes)?;
        report.wasm_sha256 = Some(model::digest(bytes));
    }
    report.compilation_seconds = start.elapsed().as_secs_f64();
    if options.verbose {
        eprintln!(
            "{}: {} ({} attempts, {:.2}s)",
            source.name(),
            report.status,
            report.attempt_count(),
            report.compilation_seconds
        );
    }
    Ok(Compilation {
        report,
        output: bytes,
    })
}

fn feedback(source: &str, attempt: &Attempt) -> String {
    format!(
        "The previous translation failed validation. Correct the WAT while preserving the original C semantics. Return the complete replacement module only.\n\nOriginal C:\n{source}\n\nPrevious response:\n{}\n\nValidation result ({}):\n{}\n",
        attempt
            .generation
            .as_ref()
            .and_then(|g| g.final_text().ok())
            .filter(|answer| !answer.trim().is_empty())
            .unwrap_or("[No final answer was produced.]"),
        attempt.status,
        attempt.error.as_deref().unwrap_or("unknown error")
    )
}

/// Only remove a single Markdown wrapper. Never fix instructions or cherry-pick a module.
pub fn extract_wat(raw: &str) -> Result<String> {
    let text = raw.trim();
    let text = if text.starts_with("```") {
        let (tag, body) = text.split_once('\n').context("incomplete code fence")?;
        ensure!(
            ["```", "```wat", "```wasm", "```webassembly"].contains(&tag),
            "unexpected code fence"
        );
        body.strip_suffix("```")
            .context("unclosed code fence")?
            .trim()
    } else {
        text
    };
    ensure!(
        text.starts_with("(module"),
        "model did not return a WAT module"
    );
    Ok(text.to_string())
}

struct LoopOutput {
    attempts: Vec<Attempt>,
    bytes: Option<Vec<u8>>,
    wat: Option<String>,
}

/// One attempt's generation and model-behavior verdict, before any persistence.
struct AttemptOutput {
    generation: Option<Generation>,
    wat: Option<String>,
    outcome: Outcome,
}

/// Generate and validate one module. Every failure here is model behavior and becomes a
/// status; this function performs no I/O, so artifact failures cannot masquerade as verdicts.
fn produce_attempt(
    source: &source::Source,
    input: &str,
    generate: &mut impl FnMut(&str) -> Result<Generation>,
) -> AttemptOutput {
    let generation = match generate(input) {
        Ok(generation) => generation,
        Err(error) => {
            return AttemptOutput {
                generation: None,
                wat: None,
                outcome: Outcome::rejected(Status::GenerationError, error),
            };
        }
    };
    if generation.stop != StopReason::Eos {
        return AttemptOutput {
            generation: Some(generation),
            wat: None,
            outcome: Outcome::rejected(
                Status::TokenLimit,
                anyhow::anyhow!(
                    "response reached its output token limit; return a complete, concise WAT module within the same budget"
                ),
            ),
        };
    }
    let wat = match generation.final_text().and_then(extract_wat) {
        Ok(wat) => wat,
        Err(error) => {
            return AttemptOutput {
                generation: Some(generation),
                wat: None,
                outcome: Outcome::rejected(Status::InvalidOutput, error),
            };
        }
    };
    let bytes = match wat::parse_str(&wat).context("assembling generated WAT") {
        Ok(bytes) => bytes,
        Err(error) => {
            return AttemptOutput {
                generation: Some(generation),
                wat: Some(wat),
                outcome: Outcome::rejected(Status::InvalidWat, error),
            };
        }
    };
    if let Err(error) = Wasm::new(&bytes, source.name()) {
        return AttemptOutput {
            generation: Some(generation),
            wat: Some(wat),
            outcome: Outcome::rejected(Status::InvalidWasm, error),
        };
    }
    AttemptOutput {
        generation: Some(generation),
        wat: Some(wat),
        outcome: Outcome::Compiled(bytes),
    }
}

/// A model-behavior verdict: success or a retryable validation failure.
/// Artifact I/O failures are not verdicts; they propagate as errors and abort compilation.
enum Outcome {
    Compiled(Vec<u8>),
    Rejected { status: Status, error: String },
}

impl Outcome {
    fn rejected(status: Status, error: anyhow::Error) -> Self {
        Self::Rejected {
            status,
            error: format!("{error:#}"),
        }
    }
}

// Keeping generation injectable lets tests exercise the actual compiler loop without a model.
fn wat_loop(
    source: &source::Source,
    max_attempts: u32,
    mut on_attempt: Option<&mut AttemptObserver<'_>>,
    verbose: bool,
    mut generate: impl FnMut(&str) -> Result<Generation>,
) -> Result<LoopOutput> {
    let mut attempts = Vec::new();
    let mut wat = None;
    let mut input = source.canonical().to_owned();
    for number in 1..=max_attempts {
        let start = Instant::now();
        let output = produce_attempt(source, &input, &mut generate);
        wat = output.wat;
        let (status, error, bytes) = match output.outcome {
            Outcome::Compiled(bytes) => (Status::Compiled, None, Some(bytes)),
            Outcome::Rejected { status, error } => (status, Some(error), None),
        };
        let attempt = Attempt {
            number,
            status,
            error,
            generation: output.generation,
            seconds: start.elapsed().as_secs_f64(),
        };
        if let Some(observer) = on_attempt.as_deref_mut() {
            observer(&attempt, &input, wat.as_deref())?;
        }
        if verbose {
            eprintln!(
                "{}: attempt {number}/{max_attempts}: {}",
                source.name(),
                attempt.status
            );
            if let Some(error) = &attempt.error {
                eprintln!("{error}");
            }
        }
        let retry = attempt.status.retryable();
        attempts.push(attempt);
        if let Some(bytes) = bytes {
            return Ok(LoopOutput {
                attempts,
                bytes: Some(bytes),
                wat,
            });
        }
        if !retry || number == max_attempts {
            break;
        }
        input = feedback(source.canonical(), attempts.last().unwrap());
    }
    Ok(LoopOutput {
        attempts,
        bytes: None,
        wat,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_guard_protects_every_sibling_not_just_the_binary() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("example.wat");
        fs::write(&source, "(module)").unwrap();
        // The .wat artifact of the default output name is the source itself.
        assert!(
            ensure_artifacts_do_not_clobber(&source, &dir.path().join("example.wasm")).is_err()
        );
        let binary = dir.path().join("plain.wasm");
        fs::write(&binary, b"").unwrap();
        assert!(ensure_artifacts_do_not_clobber(&binary, &binary).is_err());
        let other = dir.path().join("other.wasm");
        assert!(ensure_artifacts_do_not_clobber(&source, &other).is_ok());
        // Parents that do not exist yet still resolve far enough to compare identities.
        let nested = dir.path().join("nested").join("a.wasm");
        assert!(ensure_artifacts_do_not_clobber(&source, &nested).is_ok());
    }

    #[test]
    fn extraction_never_repairs_model_output() {
        assert_eq!(extract_wat("```wat\n(module)\n```").unwrap(), "(module)");
        assert!(extract_wat("Here is your code: (module)").is_err());
        assert!(wat::parse_str(extract_wat("(module)\nSome explanation").unwrap()).is_err());
    }

    fn source() -> source::Source {
        source::parse("uint32_t f(uint32_t x, uint32_t y) { return x + y; }").unwrap()
    }

    fn generation(raw: &str) -> Generation {
        Generation {
            raw: raw.into(),
            reasoning: None,
            answer: None,
            prompt_tokens: 0,
            output_tokens: 0,
            seconds: 0.0,
            stop: StopReason::Eos,
        }
    }

    const VALID: &str = "(module (func (export \"f\") (param i32 i32) (result i32) local.get 0 local.get 1 i32.add))";

    #[test]
    fn feeds_syntax_and_validation_errors_back_and_stops_on_success() {
        let src = source();
        let mut calls = 0;
        let LoopOutput {
            attempts, bytes, ..
        } = wat_loop(&src, 5, None, false, |input| {
            calls += 1;
            match calls {
                1 => {
                    assert_eq!(input, src.canonical());
                    Ok(generation("(module"))
                }
                2 => {
                    assert!(input.contains("assembling generated WAT"));
                    assert!(input.contains(src.canonical()));
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
        assert_eq!(
            attempts.iter().map(|a| a.status).collect::<Vec<_>>(),
            [Status::InvalidWat, Status::InvalidWasm, Status::Compiled]
        );
        assert_eq!(attempts.len(), 3);
        assert!(bytes.is_some());
        assert_eq!(attempts[0].generation.as_ref().unwrap().raw, "(module");
    }

    #[test]
    fn observer_receives_attempts_without_changing_results() {
        let mut seen = Vec::new();
        let mut observer = |attempt: &Attempt, input: &str, wat: Option<&str>| {
            seen.push((
                attempt.number,
                attempt.status,
                input.to_owned(),
                wat.map(str::to_owned),
            ));
            Ok(())
        };
        let run = |observer: Option<&mut AttemptObserver<'_>>| {
            let mut calls = 0;
            wat_loop(&source(), 2, observer, false, |_| {
                calls += 1;
                Ok(generation(if calls == 1 { "(module" } else { VALID }))
            })
            .unwrap()
        };
        let ordinary = run(None);
        let observed = run(Some(&mut observer));
        assert_eq!(ordinary.bytes, observed.bytes);
        assert_eq!(ordinary.wat, observed.wat);
        assert_eq!(seen.len(), 2);
        assert_eq!(
            seen[0],
            (
                1,
                Status::InvalidWat,
                source().canonical().to_owned(),
                Some("(module".into())
            )
        );
        assert_eq!(seen[1].0, 2);
        assert_eq!(seen[1].1, Status::Compiled);
        assert!(seen[1].2.contains("invalid_wat"));
        assert_eq!(seen[1].3.as_deref(), Some(VALID));
    }

    #[test]
    fn observer_errors_abort_without_retrying() {
        let mut calls = 0;
        let mut observer = |_: &Attempt, _: &str, _: Option<&str>| anyhow::bail!("observer failed");
        let result = wat_loop(&source(), 5, Some(&mut observer), false, |_| {
            calls += 1;
            Ok(generation("bad"))
        });
        assert_eq!(calls, 1);
        assert_eq!(result.err().unwrap().to_string(), "observer failed");
    }

    #[test]
    fn thinking_retries_omit_reasoning_but_preserve_raw_history() {
        for (answer, stop, expected) in [
            (Some("(module"), StopReason::Eos, Status::InvalidWat),
            (None, StopReason::TokenLimit, Status::TokenLimit),
            (None, StopReason::Eos, Status::InvalidOutput),
        ] {
            let mut calls = 0;
            let raw = format!(
                "PRIVATE_REASONING{}",
                answer.map_or(String::new(), |a| format!("</think>{a}"))
            );
            let result = wat_loop(&source(), 2, None, false, |input| {
                calls += 1;
                if calls == 1 {
                    let mut output = generation(&raw);
                    output.reasoning = Some("PRIVATE_REASONING".into());
                    output.answer = answer.map(str::to_owned);
                    output.stop = stop;
                    Ok(output)
                } else {
                    assert!(!input.contains("PRIVATE_REASONING"));
                    let previous = input
                        .split_once("Previous response:\n")
                        .unwrap()
                        .1
                        .split_once("\n\nValidation result")
                        .unwrap()
                        .0;
                    assert!(!previous.contains("</think>"));
                    assert!(input.contains(answer.unwrap_or("[No final answer was produced.]")));
                    assert!(input.contains(expected.as_str()));
                    Ok(generation(VALID))
                }
            })
            .unwrap();
            assert_eq!(result.attempts[0].status, expected);
            assert_eq!(result.attempts[1].status, Status::Compiled);
            let metadata = serde_json::to_string(&result.attempts[0]).unwrap();
            assert!(!metadata.contains("PRIVATE_REASONING"));
            assert_eq!(result.attempts[0].generation.as_ref().unwrap().raw, raw);
        }
    }

    #[test]
    fn respects_attempt_limit_and_does_not_retry_infrastructure_errors() {
        for limit in [1, 5] {
            let LoopOutput {
                attempts, bytes, ..
            } = wat_loop(&source(), limit, None, false, |_| Ok(generation("bad"))).unwrap();
            assert_eq!(attempts.len(), limit as usize);
            assert!(bytes.is_none());
        }
        let LoopOutput {
            attempts, bytes, ..
        } = wat_loop(&source(), 5, None, false, |_| {
            anyhow::bail!("inference unavailable")
        })
        .unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].status, Status::GenerationError);
        assert!(bytes.is_none());
    }

    #[test]
    fn final_attempt_artifact_does_not_reuse_an_earlier_wat() {
        let mut calls = 0;
        let result = wat_loop(&source(), 2, None, false, |_| {
            calls += 1;
            Ok(generation(if calls == 1 { "(module" } else { "not WAT" }))
        })
        .unwrap();
        assert!(result.wat.is_none());
        assert!(result.bytes.is_none());
        let result = wat_loop(&source(), 1, None, false, |_| Ok(generation(VALID))).unwrap();
        assert_eq!(result.wat.unwrap(), VALID);
    }

    #[test]
    fn valid_but_wrong_program_stops_without_semantic_feedback() {
        let LoopOutput {
            attempts, bytes, ..
        } = wat_loop(&source(), 5, None, false, |_| {
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
