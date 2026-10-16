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
    /// Assemble WAT using model-generated hex bytes, without a conventional assembler.
    Assemble {
        input: PathBuf,
        #[arg(long)]
        export: String,
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Compile once; retain the raw response and a JSON report even on failure.
    Compile {
        source: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(long, value_enum, default_value = "wat")]
        assembler: compile::Assembler,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Commands::Fetch => {
            let (path, sha) = cli.model.resolve()?;
            println!("{}\nSHA-256: {sha}", path.display());
        }
        Commands::Assemble {
            input,
            export,
            output,
        } => {
            ensure!(
                input.extension().is_some_and(|e| e == "wat"),
                "input must be a .wat file"
            );
            let text = fs::read_to_string(input)?;
            let compiler = cli.model.load()?;
            let report = neurastheniacc::assembly::assemble(&compiler, &text, export, output)?;
            ensure!(
                report.status == "assembled",
                "assembly failed; see {}",
                output.with_extension("assembly.report.json").display()
            );
        }
        Commands::Compile {
            source,
            output,
            assembler,
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
            let report = compile::compile_with_assembler(&compiler, &parsed, &output, *assembler)?;
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
