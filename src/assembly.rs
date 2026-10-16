use crate::{
    model::{self, Compiler, Generation, Settings},
    wasm::Wasm,
};
use anyhow::{Result, ensure};
use serde::Serialize;
use std::{fs, path::Path, time::Instant};

pub const PROMPT: &str = include_str!("../prompts/assembly-v1.txt");

#[derive(Serialize)]
pub struct Report {
    schema_version: u32,
    model_sha256: String,
    settings: Settings,
    prompt_sha256: String,
    input_sha256: String,
    pub input: String,
    export: String,
    pub status: String,
    pub error: Option<String>,
    pub generation: Option<Generation>,
    candidate_sha256: Option<String>,
    pub seconds: f64,
}

/// Decode bytes only. No lengths, headers, instructions, or repairs are supplied here.
pub fn decode_hex(raw: &str) -> Result<Vec<u8>> {
    let hex: Vec<u8> = raw.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    ensure!(
        !hex.is_empty() && hex.len().is_multiple_of(2),
        "expected an even, nonzero number of hex digits"
    );
    ensure!(
        hex.iter().all(|b| b.is_ascii_hexdigit()),
        "output contains non-hex text"
    );
    hex.chunks_exact(2)
        .map(|pair| Ok(u8::from_str_radix(std::str::from_utf8(pair)?, 16)?))
        .collect()
}

pub fn assemble(compiler: &Compiler, input: &str, export: &str, output: &Path) -> Result<Report> {
    ensure!(
        output.extension().is_some_and(|e| e == "wasm"),
        "output must have a .wasm extension"
    );
    ensure!(input.len() <= 32768, "WAT input exceeds 32 KiB");
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    for path in [
        output.to_path_buf(),
        output.with_extension("candidate.wasm"),
        output.with_extension("assembly.raw.txt"),
    ] {
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    fs::write(output.with_extension("assembly.input.wat"), input)?;
    fs::write(output.with_extension("assembly.prompt.txt"), PROMPT)?;
    let mut report = Report {
        schema_version: 1,
        model_sha256: compiler.model_sha256.clone(),
        settings: compiler.settings.clone(),
        prompt_sha256: model::digest(PROMPT.as_bytes()),
        input_sha256: model::digest(input.as_bytes()),
        input: input.into(),
        export: export.into(),
        status: "generation_error".into(),
        error: None,
        generation: None,
        candidate_sha256: None,
        seconds: 0.0,
    };
    let start = Instant::now();
    let result = (|| -> Result<()> {
        let generation = compiler.generate_with_prompt(PROMPT, input)?;
        fs::write(output.with_extension("assembly.raw.txt"), &generation.raw)?;
        report.generation = Some(generation);
        let generation = report.generation.as_ref().unwrap();
        report.status = "token_limit".into();
        ensure!(
            generation.stop == "eos",
            "model exhausted its output token budget"
        );
        report.status = "invalid_hex".into();
        let bytes = decode_hex(&generation.raw)?;
        report.candidate_sha256 = Some(model::digest(&bytes));
        fs::write(output.with_extension("candidate.wasm"), &bytes)?;
        report.status = "invalid_wasm".into();
        Wasm::new(&bytes, export)?;
        fs::write(output, bytes)?;
        report.status = "assembled".into();
        Ok(())
    })();
    report.seconds = start.elapsed().as_secs_f64();
    if let Err(error) = result {
        report.error = Some(format!("{error:#}"));
    }
    fs::write(
        output.with_extension("assembly.report.json"),
        serde_json::to_string_pretty(&report)?,
    )?;
    eprintln!("{export}: {} ({:.2}s)", report.status, report.seconds);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hex_decoder_never_repairs_or_discards_text() {
        assert_eq!(decode_hex("00 61\n73 6D").unwrap(), [0, 97, 115, 109]);
        for bad in [
            "",
            "0x00",
            "00 # comment",
            "```hex\n00\n```",
            "0",
            "gg",
            "00 …",
        ] {
            assert!(decode_hex(bad).is_err(), "{bad}");
        }
        assert_eq!(decode_hex("0061736d").unwrap(), [0, 97, 115, 109]);
        // Even a malformed binary must survive decoding unchanged for validation.
        assert_eq!(decode_hex("ff 00").unwrap(), [255, 0]);
    }
}
