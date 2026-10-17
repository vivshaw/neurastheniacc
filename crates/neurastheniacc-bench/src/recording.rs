//! Benchmark-owned layout and persistence of compiler attempt observations.
use anyhow::Result;
use neurastheniacc::{artifact, compile::Attempt};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub struct Recorder {
    directory: PathBuf,
    pub paths: Vec<PathBuf>,
}

impl Recorder {
    pub fn new(program_directory: &Path) -> Result<Self> {
        let directory = program_directory.join("attempts");
        // Never mix a new run with an existing attempt history.
        fs::create_dir(&directory)?;
        Ok(Self {
            directory,
            paths: Vec::new(),
        })
    }

    pub fn record(&mut self, attempt: &Attempt, input: &str, wat: Option<&str>) -> Result<()> {
        let prefix = self.directory.join(attempt.number.to_string());
        fs::create_dir(&prefix)?;
        artifact::write(prefix.join("input.txt"), input)?;
        if let Some(generation) = &attempt.generation {
            artifact::write(prefix.join("raw.txt"), &generation.raw)?;
        }
        if let Some(wat) = wat {
            artifact::write(prefix.join("output.wat"), wat)?;
        }
        artifact::write(
            prefix.join("report.json"),
            serde_json::to_string_pretty(attempt)?,
        )?;
        self.paths.push(
            PathBuf::from("attempts")
                .join(attempt.number.to_string())
                .join("report.json"),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neurastheniacc::{
        compile::Status,
        model::{Generation, StopReason},
    };

    #[test]
    fn records_raw_history_and_metadata_with_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        let mut recorder = Recorder::new(dir.path()).unwrap();
        let attempt = Attempt {
            number: 1,
            status: Status::InvalidWat,
            error: Some("assembling generated WAT".into()),
            generation: Some(Generation {
                raw: "PRIVATE_REASONING</think>(module".into(),
                reasoning: Some("PRIVATE_REASONING".into()),
                answer: Some("(module".into()),
                prompt_tokens: 12,
                output_tokens: 8,
                seconds: 0.1,
                stop: StopReason::Eos,
            }),
            seconds: 0.2,
        };
        recorder
            .record(&attempt, "source and feedback", Some("(module"))
            .unwrap();
        assert_eq!(recorder.paths, [PathBuf::from("attempts/1/report.json")]);
        let prefix = dir.path().join("attempts/1");
        assert_eq!(
            fs::read_to_string(prefix.join("input.txt")).unwrap(),
            "source and feedback"
        );
        assert_eq!(
            fs::read_to_string(prefix.join("raw.txt")).unwrap(),
            "PRIVATE_REASONING</think>(module"
        );
        assert_eq!(
            fs::read_to_string(prefix.join("output.wat")).unwrap(),
            "(module"
        );
        let text = fs::read_to_string(dir.path().join(&recorder.paths[0])).unwrap();
        assert!(!text.contains("PRIVATE_REASONING"));
        let report: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(report["status"], "invalid_wat");
        assert_eq!(report["generation"]["stop"], "eos");
        assert_eq!(report["generation"].as_object().unwrap().len(), 4);
        assert!(report.get("input").is_none());
        assert!(Recorder::new(dir.path()).is_err());
        assert!(recorder.record(&attempt, "overwrite", None).is_err());
        assert_eq!(recorder.paths.len(), 1);
    }

    #[test]
    fn generation_failure_records_diagnostics_without_fabricated_output() {
        let dir = tempfile::tempdir().unwrap();
        let mut recorder = Recorder::new(dir.path()).unwrap();
        recorder
            .record(
                &Attempt {
                    number: 1,
                    status: Status::GenerationError,
                    error: Some("inference unavailable".into()),
                    generation: None,
                    seconds: 0.0,
                },
                "source",
                None,
            )
            .unwrap();
        let prefix = dir.path().join("attempts/1");
        assert!(prefix.join("input.txt").exists());
        assert!(prefix.join("report.json").exists());
        assert!(!prefix.join("raw.txt").exists());
        assert!(!prefix.join("output.wat").exists());
    }
}
