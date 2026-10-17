mod eval;
mod recording;

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use neurastheniacc::{artifact, cli::ModelOptions, compile, source, wasm};
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Parser)]
#[command(
    version,
    about = "Run and evaluate neurastheniacc's generated WebAssembly"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
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
    /// Measure the compiler across a corpus; semantic checks do not feed back into compilation.
    Bench {
        #[command(flatten)]
        model: ModelOptions,
        #[arg(long, default_value = "fixtures/eval")]
        corpus: PathBuf,
        /// Compiler generation budget per program; 1 reproduces one-shot behavior.
        #[arg(long, default_value_t = compile::DEFAULT_ATTEMPTS, value_parser = clap::value_parser!(u32).range(1..=i64::from(compile::MAX_ATTEMPTS)))]
        attempts: u32,
        /// Add programs from the seeded template generator.
        #[arg(long, default_value_t = 0)]
        generated: u32,
        #[arg(long, default_value_t = 1000)]
        cases: u32,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Must be a new directory; defaults to benchmarks/<Unix timestamp>.
        #[arg(long)]
        out_dir: Option<PathBuf>,
    },
}

#[derive(serde::Serialize)]
struct FixtureSummary {
    name: String,
    status: eval::EvaluationStatus,
    attempts: usize,
    compilation_seconds: f64,
    report: String,
}

fn markdown_cell(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('|', "\\|")
        .replace(['\r', '\n'], " ")
}

fn fixture_row(name: &str, report: &eval::Evaluation) -> String {
    let detail = if let Some(counterexample) = report
        .check
        .as_ref()
        .and_then(|c| c.counterexample.as_ref())
    {
        format!(
            "Input {:?}: expected {}, actual {}{}",
            counterexample.inputs,
            counterexample.expected,
            counterexample
                .actual
                .map_or_else(|| "trap".into(), |v| v.to_string()),
            counterexample
                .error
                .as_ref()
                .map_or(String::new(), |e| format!(": {e}"))
        )
    } else {
        report
            .error
            .as_deref()
            .or(report.compilation.error.as_deref())
            .unwrap_or("—")
            .to_owned()
    };
    let detail = detail.lines().next().unwrap_or("—");
    let brief: String = detail.chars().take(200).collect();
    format!(
        "| {} | {} | {} | {:.2} | {} |",
        markdown_cell(name),
        report.status,
        report.compilation.attempt_count(),
        report.compilation.compilation_seconds(),
        markdown_cell(&brief)
    )
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Commands::Run { wasm, export, x, y } => {
            println!(
                "{}",
                wasm::Wasm::new(&fs::read(wasm)?, export)?.run(*x, *y)?
            );
        }
        Commands::Check {
            source,
            wasm,
            cases,
            seed,
        } => {
            let source = source::read(source)?;
            let wasm = wasm::Wasm::new(&fs::read(wasm)?, source.name())?;
            let report = eval::compare(&source, &wasm, *cases, *seed)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            ensure!(
                report.status == eval::CheckStatus::Passed,
                "differential check failed"
            );
        }
        Commands::Bench {
            model,
            corpus,
            generated,
            attempts,
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
                    source::read(&path)?,
                ));
            }
            for (name, text) in eval::generated(*generated, *seed) {
                sources.push((name, source::parse(&text)?));
            }
            ensure!(!sources.is_empty(), "empty corpus");
            // Artifact stems (e.g. 01_add) need not match exports (e.g. add).
            let names: std::collections::HashSet<_> =
                sources.iter().map(|(name, _)| name).collect();
            ensure!(
                names.len() == sources.len(),
                "duplicate corpus artifact names"
            );
            let output = out_dir.clone().unwrap_or_else(|| {
                PathBuf::from(format!(
                    "benchmarks/{}",
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
            let generator = match model.load() {
                Ok(generator) => generator,
                Err(error) => {
                    // Only remove our empty reservation, never contents from another process.
                    let _ = fs::remove_dir(&output);
                    return Err(error);
                }
            };
            let mut counts = BTreeMap::<String, usize>::new();
            let mut fixtures = Vec::new();
            let mut rows = Vec::new();
            let start = Instant::now();
            for (name, source) in sources {
                let program_directory = output.join(&name);
                fs::create_dir(&program_directory)?;
                fs::write(program_directory.join("source.c"), source.canonical())?;
                let artifact = program_directory.join("output.wasm");
                let mut recorder = recording::Recorder::new(&program_directory)?;
                let mut on_attempt =
                    |attempt: &compile::Attempt, input: &str, wat: Option<&str>| {
                        recorder.record(attempt, input, wat)
                    };
                let compilation = compile::compile(
                    &generator,
                    &source,
                    &artifact,
                    *attempts,
                    compile::Options {
                        verbose: true,
                        on_attempt: Some(&mut on_attempt),
                    },
                )?;
                let mut report = eval::evaluate(compilation, &source, *cases, *seed);
                report.attempts = recorder.paths;
                artifact::write(
                    program_directory.join("check.json"),
                    serde_json::to_string_pretty(&report)?,
                )?;
                eprintln!("{}: {}", source.name(), report.status);
                *counts.entry(report.status.to_string()).or_default() += 1;
                rows.push(fixture_row(&name, &report));
                fixtures.push(FixtureSummary {
                    report: format!("{name}/check.json"),
                    name,
                    status: report.status,
                    attempts: report.compilation.attempt_count(),
                    compilation_seconds: report.compilation.compilation_seconds(),
                });
            }
            let clang_version = std::process::Command::new("clang")
                .arg("--version")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
            let summary = serde_json::json!({
                "max_attempts": attempts, "programs": fixtures.len(), "counts": counts,
                "seed": seed, "random_inputs_per_program": cases,
                "edge_inputs_per_program": eval::inputs(0, *seed).len(),
                "benchmark_seconds_excluding_model_load": start.elapsed().as_secs_f64(),
                "clang_version": clang_version,
                "model": model.selection(),
                "settings": generator.settings(),
                "fixtures": fixtures,
            });
            artifact::write(
                output.join("summary.json"),
                serde_json::to_string_pretty(&summary)?,
            )?;
            let settings = generator.settings();
            let counts_text = counts
                .iter()
                .map(|(status, count)| format!("{count} {status}"))
                .collect::<Vec<_>>()
                .join(", ");
            let markdown = format!(
                "# Benchmark results\n\n**{}**. Programs: {}.\n\nModel: {}. Greedy decoding; chat mode: {:?}; context: {}; output budget: {}; maximum attempts: {}; seed: {}.\n\nEach passing fixture completed {} edge inputs plus {} random inputs against Clang. Checking stops at the first mismatch or runtime error. Compilation time excludes model loading and behavioral checking.\n\n| Fixture | Status | Attempts | Compile seconds | Failure detail |\n| --- | --- | ---: | ---: | --- |\n{}\n\n[Machine-readable summary](summary.json). Each fixture entry links to its detailed `check.json` report; each program directory contains its attempt history.\n",
                counts_text,
                fixtures.len(),
                markdown_cell(model.selection()),
                settings.chat_mode,
                settings.context,
                settings.max_tokens,
                attempts,
                seed,
                eval::inputs(0, *seed).len(),
                cases,
                rows.join("\n")
            );
            artifact::write(output.join("summary.md"), markdown)?;
            println!(
                "{} programs: {}\nReports: {}",
                fixtures.len(),
                serde_json::to_string(&counts)?,
                output.display()
            );
            ensure!(
                fixtures.iter().all(|fixture| fixture.status
                    == eval::EvaluationStatus::Check(eval::CheckStatus::Passed)),
                "benchmark recorded failures (see summary.json)"
            );
        }
    }
    Ok(())
}
