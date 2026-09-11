mod model;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use model::{Report, Status};

#[derive(clap::Subcommand)]
pub enum Task {
    /// Summarize a saved report without rerunning any compiler.
    Report {
        /// JSON report to summarize.
        input: PathBuf,
    },
}

pub fn run(_root: &Path, task: Task) -> anyhow::Result<()> {
    match task {
        Task::Report { input } => report(&input),
    }
}

fn report(path: &Path) -> anyhow::Result<()> {
    let contents = fs::read_to_string(path).with_context(|| path.display().to_string())?;
    let report: Report = serde_json::from_str(&contents).with_context(|| path.display().to_string())?;
    anyhow::ensure!(report.schema_version == 1, "unsupported report schema version");

    let count = |status| {
        report
            .results
            .iter()
            .filter(|result| result.status == status)
            .count()
    };
    let passed = count(Status::Pass);
    let failed = count(Status::Fail);
    let unsupported = count(Status::UnsupportedCapability);
    let missing = count(Status::MissingInfrastructure);
    println!(
        "profile={} target={} library={} pass={} fail={} unsupported={} missing_infrastructure={}",
        report.profile, report.host.target, report.host.library, passed, failed, unsupported, missing
    );
    for result in report
        .results
        .iter()
        .filter(|result| result.status != Status::Pass)
    {
        println!("{} {:?}: {}", result.case_id, result.status, result.detail);
    }
    anyhow::ensure!(
        failed == 0 && unsupported == 0 && missing == 0 && !report.results.is_empty(),
        "report contains non-passing or no results"
    );
    Ok(())
}
