use anyhow::{Result, ensure};
use clap::{Parser, Subcommand};
use neurastheniacc::{cli::ModelOptions, compile, source};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "An experimental local LLM compiler: unsigned C → WebAssembly"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
    #[command(flatten)]
    model: ModelOptions,
}

#[derive(Subcommand)]
enum Commands {
    /// Download and verify the selected model (no server required).
    Fetch,
    /// Compile C with validation feedback; retain every attempt, including failures.
    Compile {
        source: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Total generation attempts, including the initial translation.
        #[arg(long, default_value_t = compile::DEFAULT_ATTEMPTS, value_parser = clap::value_parser!(u32).range(1..=i64::from(compile::MAX_ATTEMPTS)))]
        attempts: u32,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Commands::Fetch => {
            let (path, sha) = cli.model.resolve()?;
            println!("{}\nSHA-256: {sha}", path.display());
        }
        Commands::Compile {
            source,
            output,
            attempts,
        } => {
            let parsed = source::read(source)?;
            let output = output
                .clone()
                .unwrap_or_else(|| source.with_extension("wasm"));
            // Compilation replaces every sibling artifact, so guard before loading a model.
            compile::ensure_artifacts_do_not_clobber(source, &output)?;
            ensure!(
                output.extension().is_some_and(|ext| ext == "wasm"),
                "output must have a .wasm extension"
            );
            let compiler = cli.model.load()?;
            let compilation = compile::compile(&compiler, &parsed, &output, *attempts)?;
            ensure!(
                compilation.report.status == compile::Status::Compiled,
                "compilation failed; see {}",
                compile::Artifacts::new(&output).report.display()
            );
            println!("Wrote {} ({})", output.display(), compilation.report.status);
        }
    }
    Ok(())
}
