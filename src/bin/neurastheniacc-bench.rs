use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use neurastheniacc::{cli::ModelOptions, compile, eval, source, wasm};
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
    /// Compare model-assembled binaries against wat on fixed WAT fixtures exporting f.
    Assembly {
        #[command(flatten)]
        model: ModelOptions,
        #[arg(long, default_value = "fixtures/assembly")]
        corpus: PathBuf,
        #[arg(long)]
        out_dir: PathBuf,
        #[arg(long, default_value_t = 1000)]
        cases: u32,
        #[arg(long, default_value_t = 20261015)]
        seed: u64,
    },
    /// Measure first-attempt results across a corpus. No repair or fallback.
    Bench {
        #[command(flatten)]
        model: ModelOptions,
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Commands::Assembly {
            model,
            corpus,
            out_dir,
            cases,
            seed,
        } => {
            let compiler = model.load()?;
            neurastheniacc::assembly_eval::bench(&compiler, corpus, out_dir, *cases, *seed)?;
        }
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
            let wasm = wasm::Wasm::new(&fs::read(wasm)?, &source.name)?;
            let report = eval::compare(&source, &wasm, *cases, *seed)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            ensure!(report.status == "passed", "differential check failed");
        }
        Commands::Bench {
            model,
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
                    source::read(&path)?,
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
            let compiler = model.load()?;
            let mut counts = BTreeMap::<String, usize>::new();
            let mut reports = Vec::new();
            let start = Instant::now();
            for (name, source) in sources {
                fs::write(output.join(format!("{name}.c")), &source.canonical)?;
                let artifact = output.join(format!("{name}.wasm"));
                let compilation = compile::compile_one(&compiler, &source, &artifact)?;
                let report = eval::evaluate(compilation, &source, &artifact, *cases, *seed);
                fs::write(
                    artifact.with_extension("check.json"),
                    serde_json::to_string_pretty(&report)?,
                )?;
                eprintln!("{}: {}", source.name, report.status);
                *counts.entry(report.status.clone()).or_default() += 1;
                reports.push(report);
            }
            let clang_version = std::process::Command::new("clang")
                .arg("--version")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
            let summary = serde_json::json!({
                "schema_version": 2, "programs": reports.len(), "counts": counts,
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
