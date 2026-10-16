use crate::{assembly, eval, model::Compiler, wasm::Wasm};
use anyhow::{Result, ensure};
use serde_json::json;
use std::{collections::BTreeMap, fs, path::Path};

pub fn bench(
    compiler: &Compiler,
    corpus: &Path,
    output: &Path,
    cases: u32,
    seed: u64,
    max_attempts: u32,
) -> Result<()> {
    ensure!(cases <= 1_000_000, "benchmark size exceeds limits");
    let mut paths = fs::read_dir(corpus)?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.retain(|p| p.extension().is_some_and(|e| e == "wat"));
    paths.sort();
    ensure!(!paths.is_empty(), "empty assembly corpus");
    // Validate every reference before generating anything. Reference bytes never enter the prompt.
    let mut references = Vec::new();
    for path in paths {
        let input = fs::read_to_string(&path)?;
        let bytes = wat::parse_str(&input)?;
        let module = Wasm::new(&bytes, "f")?;
        references.push((path, input, bytes, module));
    }
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(output)?;
    let inputs = eval::inputs(cases, seed);
    let mut reports = Vec::new();
    let mut counts = BTreeMap::<String, usize>::new();
    for (path, input, reference_bytes, reference) in references {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let artifact = output.join(format!("{name}.wasm"));
        let compilation = assembly::assemble(compiler, &input, "f", &artifact, max_attempts)?;
        fs::write(
            output.join(format!("{name}.reference.wasm")),
            &reference_bytes,
        )?;
        let mut status = compilation.status.clone();
        let mut executed = 0;
        let mut counterexample = None;
        let mut byte_equal = None;
        if status == "assembled" {
            let candidate_bytes = fs::read(&artifact)?;
            byte_equal = Some(candidate_bytes == reference_bytes);
            let candidate = Wasm::new(&candidate_bytes, "f")?;
            status = "passed".into();
            for &[x, y] in &inputs {
                let expected = reference.run(x, y)?;
                executed += 1;
                match candidate.run(x, y) {
                    Ok(actual) if actual == expected => (),
                    result => {
                        status = if result.is_err() {
                            "runtime_error"
                        } else {
                            "mismatch"
                        }
                        .into();
                        counterexample = Some(json!({"inputs":[x,y],"expected":expected,
                            "actual":result.as_ref().ok(),"error":result.err().map(|e|e.to_string())}));
                        break;
                    }
                }
            }
        }
        eprintln!("{name}: {status}");
        *counts.entry(status.clone()).or_default() += 1;
        let report = json!({"fixture":name,"status":status,"assembly":compilation,
            "byte_equal":byte_equal,"planned_inputs":inputs.len(),"executed_inputs":executed,"counterexample":counterexample});
        fs::write(
            artifact.with_extension("assembly.check.json"),
            serde_json::to_string_pretty(&report)?,
        )?;
        reports.push(report);
    }
    let passed = counts.get("passed").copied().unwrap_or(0);
    fs::write(
        output.join("summary.json"),
        serde_json::to_string_pretty(&json!({
            "schema_version":2,"max_attempts":max_attempts,"programs":reports.len(),"counts":counts,"seed":seed,"random_inputs":cases,
            "model_load_seconds":compiler.load_seconds,"reports":reports
        }))?,
    )?;
    println!(
        "{} programs: {}",
        reports.len(),
        serde_json::to_string(&counts)?
    );
    ensure!(
        passed == reports.len(),
        "assembly benchmark recorded failures (see summary.json)"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frozen_fixtures_have_expected_behavior() {
        let fixtures = [
            (include_str!("../fixtures/assembly/01_add.wat"), 8),
            (
                include_str!("../fixtures/assembly/02_signed_leb.wat"),
                !131u32,
            ),
            (include_str!("../fixtures/assembly/03_locals.wat"), 104),
            (include_str!("../fixtures/assembly/04_branch.wat"), 5),
            (
                include_str!("../fixtures/assembly/05_function_indices.wat"),
                9,
            ),
            (include_str!("../fixtures/assembly/06_long_body.wat"), 8),
        ];
        for (input, expected) in fixtures {
            let bytes = wat::parse_str(input).unwrap();
            assert_eq!(Wasm::new(&bytes, "f").unwrap().run(3, 5).unwrap(), expected);
        }
    }
}
