use anyhow::{Result, ensure};
use clap::{Parser, Subcommand};
use neurastheniacc::{cli::ModelOptions, compile, source};
use std::{fs, path::PathBuf};

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
        #[arg(long, default_value_t = compile::DEFAULT_ATTEMPTS, value_parser = clap::value_parser!(u32).range(1..=5))]
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
            if output.exists() {
                ensure!(
                    fs::canonicalize(source)? != fs::canonicalize(&output)?,
                    "output must not overwrite source"
                );
            }
            let compiler = cli.model.load()?;
            let report = compile::compile_with_options(&compiler, &parsed, &output, *attempts)?;
            ensure!(
                report.status == "compiled",
                "compilation failed; see {}",
                output.with_extension("report.json").display()
            );
            println!("Wrote {} ({})", output.display(), report.status);
        }
    }
    Ok(())
}
