use crate::{
    model::{self, Compiler, Generation, Settings},
    wasm::Wasm,
};
use anyhow::{Result, ensure};
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

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
    candidate_sha256: Option<String>,
    seconds: f64,
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

pub fn assemble(
    compiler: &Compiler,
    input: &str,
    export: &str,
    output: &Path,
    max_attempts: u32,
) -> Result<Report> {
    ensure!(
        (1..=5).contains(&max_attempts),
        "attempts must be between 1 and 5"
    );
    ensure!(
        output.extension().is_some_and(|e| e == "wasm"),
        "output must have a .wasm extension"
    );
    ensure!(input.len() <= 32768, "WAT input exceeds 32 KiB");
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let attempt_directory = tempfile::Builder::new()
        .prefix(&format!(
            "{}.assembly.attempts-",
            output.file_stem().unwrap().to_string_lossy()
        ))
        .tempdir_in(parent)?
        .keep();
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
    fs::write(attempt_directory.join("system.prompt.txt"), PROMPT)?;
    let start = Instant::now();
    let (attempts, bytes) =
        binary_loop(input, export, max_attempts, &attempt_directory, |request| {
            compiler.generate_with_prompt(PROMPT, request)
        })?;
    let last = attempts.last().unwrap();
    if let Some(generation) = &last.generation {
        fs::write(output.with_extension("assembly.raw.txt"), &generation.raw)?;
    }
    let candidate = attempt_directory.join(format!("attempt-{}.candidate.wasm", last.number));
    if candidate.exists() {
        fs::copy(candidate, output.with_extension("candidate.wasm"))?;
    }
    if let Some(bytes) = bytes {
        fs::write(output, bytes)?;
    }
    let report = Report {
        schema_version: 2,
        model_sha256: compiler.model_sha256.clone(),
        settings: compiler.settings.clone(),
        prompt_sha256: model::digest(PROMPT.as_bytes()),
        input_sha256: model::digest(input.as_bytes()),
        input: input.into(),
        export: export.into(),
        status: last.status.clone(),
        error: last.error.clone(),
        generation: last.generation.clone(),
        candidate_sha256: last.candidate_sha256.clone(),
        seconds: start.elapsed().as_secs_f64(),
        max_attempts,
        attempt_directory,
        attempts,
    };
    fs::write(
        output.with_extension("assembly.report.json"),
        serde_json::to_string_pretty(&report)?,
    )?;
    fs::write(
        report.attempt_directory.join("report.json"),
        serde_json::to_string_pretty(&report)?,
    )?;
    eprintln!(
        "{export}: {} ({} attempts, {:.2}s)",
        report.status,
        report.attempts.len(),
        report.seconds
    );
    Ok(report)
}

fn binary_loop(
    input: &str,
    export: &str,
    max_attempts: u32,
    directory: &Path,
    mut generate: impl FnMut(&str) -> Result<Generation>,
) -> Result<(Vec<Attempt>, Option<Vec<u8>>)> {
    ensure!(
        (1..=5).contains(&max_attempts),
        "attempts must be between 1 and 5"
    );
    let mut request = input.to_owned();
    let mut attempts = Vec::new();
    for number in 1..=max_attempts {
        let prefix = directory.join(format!("attempt-{number}"));
        fs::write(prefix.with_extension("input.txt"), &request)?;
        let start = Instant::now();
        let mut attempt = Attempt {
            number,
            input: request.clone(),
            status: "generation_error".into(),
            error: None,
            generation: None,
            candidate_sha256: None,
            seconds: 0.0,
        };
        let result = (|| -> Result<Vec<u8>> {
            attempt.generation = Some(generate(&request)?);
            let generation = attempt.generation.as_ref().unwrap();
            fs::write(prefix.with_extension("raw.txt"), &generation.raw)?;
            attempt.status = "token_limit".into();
            ensure!(
                generation.stop == "eos",
                "response reached its output token limit; emit the complete binary within the same budget, without prose or repetition"
            );
            attempt.status = "invalid_hex".into();
            let bytes = decode_hex(&generation.raw)?;
            attempt.candidate_sha256 = Some(model::digest(&bytes));
            fs::write(prefix.with_extension("candidate.wasm"), &bytes)?;
            attempt.status = "invalid_wasm".into();
            Wasm::new(&bytes, export)?;
            attempt.status = "assembled".into();
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
            "{export}: assembly attempt {number}/{max_attempts}: {}",
            attempt.status
        );
        let retry = matches!(
            attempt.status.as_str(),
            "invalid_hex" | "invalid_wasm" | "token_limit"
        );
        request = format!(
            "The previous binary failed validation. Assemble the original WAT again, correcting the reported error while preserving all instructions and semantics. Return the complete binary as hex digits only. Do not return a patch. The required export is {export} with signature (i32, i32) -> i32.\n\nOriginal WAT:\n{input}\n\nPrevious response:\n{}\n\nValidation result ({}):\n{}\n",
            attempt.generation.as_ref().map_or("", |g| g.raw.as_str()),
            attempt.status,
            attempt.error.as_deref().unwrap_or("unknown error")
        );
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
    fn valid_hex() -> String {
        wat::parse_str("(module (func (export \"f\") (param i32 i32) (result i32) i32.const 0))")
            .unwrap()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
    #[test]
    fn passes_decoder_and_binary_diagnostics_back_then_stops_on_valid_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let original = include_str!("../fixtures/assembly/01_add.wat");
        let mut calls = 0;
        let (attempts, bytes) = binary_loop(original, "f", 5, dir.path(), |input| {
            calls += 1;
            match calls {
                1 => {
                    assert_eq!(input, original);
                    Ok(generation("not hex"))
                }
                2 => {
                    assert!(input.contains("non-hex"));
                    assert!(input.contains(original));
                    Ok(generation("ff00"))
                }
                3 => {
                    assert!(input.contains("invalid_wasm"));
                    assert!(input.contains("ff00"));
                    Ok(generation(&valid_hex()))
                }
                _ => panic!("retried a valid binary"),
            }
        })
        .unwrap();
        assert_eq!(attempts.len(), 3);
        // Valid but semantically wrong binaries stop the loop; tests never feed back.
        assert_eq!(
            Wasm::new(&bytes.unwrap(), "f").unwrap().run(3, 5).unwrap(),
            0
        );
        assert_eq!(
            fs::read(dir.path().join("attempt-2.candidate.wasm")).unwrap(),
            [255, 0]
        );
    }
    #[test]
    fn binary_loop_bounds_retries_and_stops_on_inference_failure() {
        let dir = tempfile::tempdir().unwrap();
        for limit in [1, 5] {
            let (attempts, bytes) = binary_loop("(module)", "f", limit, dir.path(), |_| {
                Ok(generation("ff00"))
            })
            .unwrap();
            assert_eq!(attempts.len(), limit as usize);
            assert!(bytes.is_none());
        }
        let (attempts, _) = binary_loop("(module)", "f", 5, dir.path(), |_| {
            anyhow::bail!("inference unavailable")
        })
        .unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].status, "generation_error");
        let mut calls = 0;
        let (attempts, bytes) = binary_loop("(module)", "f", 5, dir.path(), |input| {
            calls += 1;
            let mut g = generation(&valid_hex());
            if calls == 1 {
                g.stop = "token_limit";
            } else {
                assert!(input.contains("output token limit"));
            }
            Ok(g)
        })
        .unwrap();
        assert_eq!(attempts.len(), 2);
        assert!(bytes.is_some());
    }
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
