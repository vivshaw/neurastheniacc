mod eval;
mod model;
mod source;

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use model::{Compiler, Generation, Settings};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Parser)]
#[command(
    version,
    about = "An experimental local LLM compiler: unsigned C → WebAssembly"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
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

#[derive(Subcommand)]
enum Commands {
    /// Download and verify the default model (no server required).
    Fetch,
    /// Compile once; retain the raw response and a JSON report even on failure.
    Compile {
        source: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Compare against Clang on edge cases and random inputs.
        #[arg(long)]
        check: bool,
        #[arg(long, default_value_t = 1000)]
        cases: u32,
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
    /// Execute an existing .wasm without loading a model.
    Run {
        wasm: PathBuf,
        #[arg(long)]
        export: String,
        x: u32,
        y: u32,
    },
    /// Compare an existing .wasm against Clang without loading a model.
    Check {
        source: PathBuf,
        wasm: PathBuf,
        #[arg(long, default_value_t = 1000)]
        cases: u32,
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
    /// Measure first-attempt results across a corpus. No repair or fallback.
    Bench {
        #[arg(long, default_value = "fixtures/eval")]
        corpus: PathBuf,
        /// Add programs from the seeded template generator.
        #[arg(long, default_value_t = 0)]
        generated: u32,
        #[arg(long, default_value_t = 1000)]
        cases: u32,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Must be a new directory; defaults to runs/<Unix timestamp>.
        #[arg(long)]
        out_dir: Option<PathBuf>,
    },
}

#[derive(Serialize)]
struct Report {
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
    status: String,
    error: Option<String>,
    generation: Option<Generation>,
    wasm_sha256: Option<String>,
    check: Option<eval::Check>,
}

fn load(cli: &Cli) -> Result<Compiler> {
    let cache = cli.cache_dir.clone().unwrap_or_else(model::default_cache);
    let (path, sha) = model::resolve(cli.model.as_deref(), &cache, cli.offline)?;
    let prompt = match &cli.prompt {
        Some(path) => fs::read_to_string(path)?,
        None => model::DEFAULT_PROMPT.into(),
    };
    eprintln!("Loading {} (in-process)…", path.display());
    Compiler::load(
        &path,
        sha,
        Settings {
            context: cli.context,
            max_tokens: cli.max_tokens,
            threads: cli.threads,
            gpu_layers: cli.gpu_layers,
        },
        prompt,
    )
}

fn read_source(path: &Path) -> Result<source::Source> {
    source::parse(&fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?)
        .with_context(|| format!("unsupported source: {}", path.display()))
}

fn compile_one(
    compiler: &Compiler,
    source: &source::Source,
    output: &Path,
    check: bool,
    cases: u32,
    seed: u64,
) -> Result<Report> {
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
        schema_version: 1,
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
        check: None,
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
        let wasm = eval::Wasm::new(&bytes, &source.name)?;
        report.wasm_sha256 = Some(model::digest(&bytes));
        report.compilation_seconds = start.elapsed().as_secs_f64();
        if check {
            report.status = "reference_error".into();
            let checked = eval::compare(source, &wasm, cases, seed)?;
            report.status = checked.status.into();
            report.check = Some(checked);
        } else {
            report.status = "compiled_unchecked".into();
        }
        // Preserve valid candidates even when differential testing finds a counterexample.
        fs::write(output, &bytes)?;
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
    if let Some(example) = report
        .check
        .as_ref()
        .and_then(|c| c.counterexample.as_ref())
    {
        eprintln!("  counterexample: {}", serde_json::to_string(example)?);
    }
    Ok(report)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Commands::Fetch => {
            let cache = cli.cache_dir.clone().unwrap_or_else(model::default_cache);
            let (path, sha) = model::resolve(cli.model.as_deref(), &cache, cli.offline)?;
            println!("{}\nSHA-256: {sha}", path.display());
        }
        Commands::Compile {
            source,
            output,
            check,
            cases,
            seed,
        } => {
            ensure!(*cases <= 1_000_000, "at most one million random inputs");
            let parsed = read_source(source)?;
            let output = output
                .clone()
                .unwrap_or_else(|| source.with_extension("wasm"));
            if output.exists() {
                ensure!(
                    fs::canonicalize(source)? != fs::canonicalize(&output)?,
                    "output must not overwrite source"
                );
            }
            let compiler = load(&cli)?;
            let report = compile_one(&compiler, &parsed, &output, *check, *cases, *seed)?;
            ensure!(
                ["passed", "compiled_unchecked"].contains(&report.status.as_str()),
                "compilation/check failed; see {}",
                output.with_extension("report.json").display()
            );
            println!("Wrote {} ({})", output.display(), report.status);
        }
        Commands::Run { wasm, export, x, y } => {
            println!(
                "{}",
                eval::Wasm::new(&fs::read(wasm)?, export)?.run(*x, *y)?
            );
        }
        Commands::Check {
            source,
            wasm,
            cases,
            seed,
        } => {
            let source = read_source(source)?;
            let wasm = eval::Wasm::new(&fs::read(wasm)?, &source.name)?;
            let report = eval::compare(&source, &wasm, *cases, *seed)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            ensure!(report.status == "passed", "differential check failed");
        }
        Commands::Bench {
            corpus,
            generated,
            cases,
            seed,
            out_dir,
        } => {
            ensure!(
                *generated <= 1000 && *cases <= 1_000_000,
                "benchmark size exceeds limits"
            );
            let mut paths = fs::read_dir(corpus)
                .with_context(|| format!("reading corpus {}", corpus.display()))?
                .map(|entry| entry.map(|e| e.path()))
                .collect::<std::io::Result<Vec<_>>>()?;
            paths.retain(|p| p.extension().is_some_and(|e| e == "c"));
            paths.sort();
            let mut sources = Vec::new();
            for path in paths {
                sources.push((
                    path.file_stem().unwrap().to_string_lossy().into_owned(),
                    read_source(&path)?,
                ));
            }
            for (name, text) in eval::generated(*generated, *seed) {
                sources.push((name, source::parse(&text)?));
            }
            ensure!(!sources.is_empty(), "empty corpus");
            let names: std::collections::HashSet<_> =
                sources.iter().map(|(name, _)| name).collect();
            ensure!(
                names.len() == sources.len(),
                "duplicate corpus artifact names"
            );
            let output = out_dir.clone().unwrap_or_else(|| {
                PathBuf::from(format!(
                    "runs/{}",
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                ))
            });
            if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
                fs::create_dir_all(parent)?;
            }
            fs::create_dir(&output).context("benchmark output must be a new directory")?;
            let compiler = load(&cli)?;
            let mut counts = BTreeMap::<String, usize>::new();
            let mut reports = Vec::new();
            let start = Instant::now();
            for (name, source) in sources {
                fs::write(output.join(format!("{name}.c")), &source.canonical)?;
                let report = compile_one(
                    &compiler,
                    &source,
                    &output.join(format!("{name}.wasm")),
                    true,
                    *cases,
                    *seed,
                )?;
                *counts.entry(report.status.clone()).or_default() += 1;
                reports.push(report);
            }
            let clang_version = std::process::Command::new("clang")
                .arg("--version")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
            let summary = serde_json::json!({
                "schema_version": 1, "programs": reports.len(), "counts": counts,
                "seed": seed, "random_inputs_per_program": cases,
                "edge_inputs_per_program": eval::inputs(0, *seed).len(),
                "benchmark_seconds_excluding_model_load": start.elapsed().as_secs_f64(),
                "clang_version": clang_version,
                "reports": reports,
            });
            fs::write(
                output.join("summary.json"),
                serde_json::to_string_pretty(&summary)?,
            )?;
            println!(
                "{} programs: {}\nReports: {}",
                reports.len(),
                serde_json::to_string(&counts)?,
                output.display()
            );
            ensure!(
                counts.get("passed").copied().unwrap_or(0) == reports.len(),
                "benchmark recorded failures (see summary.json)"
            );
        }
    }
    Ok(())
}
