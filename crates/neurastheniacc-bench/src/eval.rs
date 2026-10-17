use anyhow::{Context, Result, ensure};
use neurastheniacc::source::Source;
use neurastheniacc::wasm::Wasm;
use serde::Serialize;
use std::{
    fs::{self, File},
    io::Write,
    process::{Command, Stdio},
    time::Instant,
};
/// Fixed algorithm rather than a dependency's changing RNG: reproducible corpus and inputs.
struct Rng(u64);
impl Rng {
    fn next_u32(&mut self) -> u32 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        (z ^ (z >> 31)) as u32
    }
}

pub fn inputs(random: u32, seed: u64) -> Vec<[u32; 2]> {
    let edges = [
        0,
        1,
        2,
        7,
        31,
        32,
        33,
        99,
        100,
        101,
        127,
        255,
        65535,
        0x7fffffff,
        0x80000000,
        0xffff0000,
        u32::MAX - 1,
        u32::MAX,
    ];
    let mut cases = Vec::new();
    for x in edges {
        for y in edges {
            cases.push([x, y]);
        }
    }
    let mut rng = Rng(seed);
    for _ in 0..random {
        cases.push([rng.next_u32(), rng.next_u32()]);
    }
    cases
}

pub fn reference(source: &Source, cases: &[[u32; 2]]) -> Result<Vec<u32>> {
    let dir = tempfile::tempdir()?;
    let c_path = dir.path().join("reference.c");
    let subject_path = dir.path().join("subject.c");
    let exe = dir.path().join("reference");
    // Separate translation units keep stdio macros and harness identifiers out of the source.
    // There are no calls in the supported subset, so renaming only the function is sufficient.
    // A typedef, not <stdint.h>, provides uint32_t: header macros such as UINT32_MAX must not
    // collide with identifiers the front end accepted.
    let subject = source.renamed("neurastheniacc_subject")?;
    fs::write(
        &subject_path,
        format!(
            "typedef unsigned int uint32_t;\n\
             _Static_assert(sizeof(uint32_t) == 4, \"requires 32-bit unsigned int\");\n\
             {subject}\n"
        ),
    )?;
    let driver = r#"#include <stdint.h>
#include <inttypes.h>
#include <stdio.h>
_Static_assert(sizeof(unsigned int) == 4, "requires 32-bit unsigned int");
extern uint32_t neurastheniacc_subject(uint32_t, uint32_t);
int main(void) {
    uint32_t ncc_x, ncc_y;
    while (scanf("%" SCNu32 " %" SCNu32, &ncc_x, &ncc_y) == 2) {
        printf("%" PRIu32 "\n", neurastheniacc_subject(ncc_x, ncc_y));
    }
    return 0;
}
"#;
    fs::write(&c_path, driver)?;
    // Only the front end restricts the language; Clang warnings must not reject supported code.
    let result = Command::new("clang")
        .args(["-std=c11", "-O2"])
        .arg(&c_path)
        .arg(&subject_path)
        .arg("-o")
        .arg(&exe)
        .output()
        .context("Clang is required for differential checking")?;
    ensure!(
        result.status.success(),
        "reference compiler failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let input_path = dir.path().join("inputs");
    let mut file = File::create(&input_path)?;
    for [x, y] in cases {
        writeln!(file, "{x} {y}")?;
    }
    drop(file);
    let result = Command::new(&exe)
        .stdin(Stdio::from(File::open(input_path)?))
        .output()?;
    ensure!(result.status.success(), "reference executable failed");
    let values = String::from_utf8(result.stdout)?
        .lines()
        .map(str::parse)
        .collect::<std::result::Result<Vec<u32>, _>>()?;
    ensure!(
        values.len() == cases.len(),
        "reference returned the wrong number of results"
    );
    Ok(values)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Passed,
    Mismatch,
    RuntimeError,
    CheckError,
}

impl CheckStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Mismatch => "mismatch",
            Self::RuntimeError => "runtime_error",
            Self::CheckError => "check_error",
        }
    }
}

// Preserve the existing flat JSON status while keeping compilation and checking distinct.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum EvaluationStatus {
    Compilation(neurastheniacc::compile::Status),
    Check(CheckStatus),
}

impl std::fmt::Display for EvaluationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Compilation(status) => status.as_str(),
            Self::Check(status) => status.as_str(),
        })
    }
}

#[derive(Serialize, Debug)]
pub struct Check {
    pub status: CheckStatus,
    pub total_inputs: usize,
    pub executed_inputs: usize,
    pub seed: u64,
    pub counterexample: Option<Counterexample>,
    pub reference_seconds: f64,
    /// Includes fresh sandbox instantiation per input; not native-code throughput.
    pub wasm_seconds: f64,
}

#[derive(Serialize, Debug)]
pub struct Counterexample {
    pub inputs: [u32; 2],
    pub expected: u32,
    pub actual: Option<u32>,
    pub error: Option<String>,
}

pub fn compare(source: &Source, wasm: &Wasm, random: u32, seed: u64) -> Result<Check> {
    ensure!(random <= 1_000_000, "at most one million random inputs");
    let cases = inputs(random, seed);
    let start = Instant::now();
    let expected = reference(source, &cases)?;
    let reference_seconds = start.elapsed().as_secs_f64();
    let start = Instant::now();
    let mut check = Check {
        status: CheckStatus::Passed,
        total_inputs: cases.len(),
        executed_inputs: 0,
        seed,
        counterexample: None,
        reference_seconds,
        wasm_seconds: 0.0,
    };
    for (input, expected) in cases.into_iter().zip(expected) {
        check.executed_inputs += 1;
        match wasm.run(input[0], input[1]) {
            Ok(actual) if actual == expected => {}
            Ok(actual) => {
                check.status = CheckStatus::Mismatch;
                check.counterexample = Some(Counterexample {
                    inputs: input,
                    expected,
                    actual: Some(actual),
                    error: None,
                });
                break;
            }
            Err(error) => {
                check.status = CheckStatus::RuntimeError;
                check.counterexample = Some(Counterexample {
                    inputs: input,
                    expected,
                    actual: None,
                    error: Some(format!("{error:#}")),
                });
                break;
            }
        }
    }
    check.wasm_seconds = start.elapsed().as_secs_f64();
    Ok(check)
}

pub fn generated(count: u32, seed: u64) -> Vec<(String, String)> {
    let mut rng = Rng(seed);
    (0..count).map(|i| {
        let a = rng.next_u32();
        let b = rng.next_u32();
        let shift = rng.next_u32() % 32;
        let op = ["+", "-", "*", "^", "&", "|"][rng.next_u32() as usize % 6];
        let cmp = ["<", ">", "<=", ">=", "==", "!="][rng.next_u32() as usize % 6];
        let body = match i % 4 {
            0 => format!("return (x {op} {a}u) ^ (y + {b}u);"),
            1 => format!("uint32_t z = (x + y) >> {shift}u; if (z {cmp} {a}u) {{ return z * {b}u; }} return ~z;"),
            2 => format!("uint32_t z = x {op} y; if (x {cmp} y) {{ z = z + {a}u; }} else {{ z = z - {b}u; }} return z;"),
            _ => format!("if (x {cmp} {a}u) {{ if (y > {b}u) {{ return x; }} return y; }} return (x << {shift}u) ^ y;"),
        };
        let name = format!("generated_{i:03}");
        let src = format!("uint32_t {name}(uint32_t x, uint32_t y) {{ {body} }}\n");
        (name, src)
    }).collect()
}

/// Harness outcome is separate from compilation success and its saved report.
#[derive(Serialize)]
pub struct Evaluation {
    pub status: EvaluationStatus,
    pub compilation: neurastheniacc::compile::Report,
    /// Attempt reports relative to the program directory; owned by the harness.
    pub attempts: Vec<std::path::PathBuf>,
    pub check: Option<Check>,
    pub error: Option<String>,
}

pub fn evaluate(
    compilation: neurastheniacc::compile::Compilation,
    source: &Source,
    cases: u32,
    seed: u64,
) -> Evaluation {
    let neurastheniacc::compile::Compilation { report, output } = compilation;
    let mut result = Evaluation {
        status: EvaluationStatus::Compilation(report.status),
        compilation: report,
        attempts: Vec::new(),
        check: None,
        error: None,
    };
    if result.compilation.status != neurastheniacc::compile::Status::Compiled {
        return result;
    }
    match (|| -> Result<Check> {
        let bytes = output.context("compiled report without module bytes")?;
        let wasm = Wasm::new(&bytes, source.name())?;
        compare(source, &wasm, cases, seed)
    })() {
        Ok(check) => {
            result.status = EvaluationStatus::Check(check.status);
            result.check = Some(check);
        }
        Err(error) => {
            result.status = EvaluationStatus::Check(CheckStatus::CheckError);
            result.error = Some(format!("{error:#}"));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_statuses_preserve_flat_report_values() {
        use neurastheniacc::{compile::Status, model::StopReason};
        for status in [
            Status::Compiled,
            Status::GenerationError,
            Status::TokenLimit,
            Status::InvalidOutput,
            Status::InvalidWat,
            Status::InvalidWasm,
        ] {
            let expected = serde_json::json!(status.as_str());
            assert_eq!(serde_json::to_value(status).unwrap(), expected);
            assert_eq!(
                serde_json::to_value(EvaluationStatus::Compilation(status)).unwrap(),
                expected
            );
        }
        for status in [
            CheckStatus::Passed,
            CheckStatus::Mismatch,
            CheckStatus::RuntimeError,
            CheckStatus::CheckError,
        ] {
            assert_eq!(
                serde_json::to_value(EvaluationStatus::Check(status)).unwrap(),
                serde_json::json!(status.as_str())
            );
        }
        assert_eq!(serde_json::to_value(StopReason::Eos).unwrap(), "eos");
        assert_eq!(
            serde_json::to_value(StopReason::TokenLimit).unwrap(),
            "token_limit"
        );
    }

    #[test]
    fn differential_check_catches_signed_comparisons() {
        let source = neurastheniacc::source::parse(
            "uint32_t f(uint32_t x, uint32_t y) { if (x > y) { return x; } return y; }",
        )
        .unwrap();
        for (op, status) in [
            ("gt_u", CheckStatus::Passed),
            ("gt_s", CheckStatus::Mismatch),
        ] {
            let bytes = wat::parse_str(format!("(module (func (export \"f\") (param i32 i32) (result i32) local.get 0 local.get 1 i32.{op} if (result i32) local.get 0 else local.get 1 end))")).unwrap();
            let check = compare(&source, &Wasm::new(&bytes, "f").unwrap(), 32, 1).unwrap();
            assert_eq!(check.status, status);
        }
    }
    #[test]
    fn rejects_imports_start_and_wrong_signature() {
        for wat in [
            "(module (import \"host\" \"f\" (func)))",
            "(module (func $s) (start $s))",
            "(module (func (export \"f\")))",
            "(module (global (mut i32) (i32.const 0)) (func (export \"f\") (param i32 i32) (result i32) i32.const 0))",
            "(module (memory 1) (func (export \"f\") (param i32 i32) (result i32) i32.const 0))",
        ] {
            assert!(Wasm::new(&wat::parse_str(wat).unwrap(), "f").is_err());
        }
    }
    #[test]
    fn execution_is_bounded_and_unsigned() {
        let infinite = wat::parse_str("(module (func (export \"f\") (param i32 i32) (result i32) (loop $l br $l) unreachable))").unwrap();
        assert!(Wasm::new(&infinite, "f").unwrap().run(0, 0).is_err());
        let add = wat::parse_str("(module (func (export \"f\") (param i32 i32) (result i32) local.get 0 local.get 1 i32.add))").unwrap();
        assert_eq!(Wasm::new(&add, "f").unwrap().run(u32::MAX, 2).unwrap(), 1);
    }
    #[test]
    fn oracle_does_not_pollute_the_source_namespace() {
        let source = neurastheniacc::source::parse(
            "uint32_t ncc_x(uint32_t EOF, uint32_t y) { return EOF + y; }",
        )
        .unwrap();
        assert_eq!(reference(&source, &[[3, 5]]).unwrap(), vec![8]);
    }
    #[test]
    fn oracle_tolerates_identifiers_that_stdint_macros_would_clobber() {
        for name in ["UINT32_MAX", "UINT32_C", "INT32_MIN"] {
            let text = format!("uint32_t f(uint32_t {name}, uint32_t y) {{ return {name} + y; }}");
            let source = neurastheniacc::source::parse(&text).unwrap();
            assert_eq!(reference(&source, &[[3, 5]]).unwrap(), vec![8]);
        }
    }
    #[test]
    fn oracle_tolerates_code_the_front_end_accepts() {
        let source = neurastheniacc::source::parse(
            "uint32_t f(uint32_t x, uint32_t y) { uint32_t unused = x; return y; }",
        )
        .unwrap();
        assert_eq!(reference(&source, &[[3, 5]]).unwrap(), vec![5]);
    }
    #[test]
    fn generated_c_is_supported_and_compiles() {
        for (_, text) in generated(8, 42) {
            let source = neurastheniacc::source::parse(&text).unwrap();
            assert_eq!(
                reference(&source, &[[0, 0], [u32::MAX, 1]]).unwrap().len(),
                2
            );
        }
    }
}
