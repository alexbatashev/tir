mod model;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use model::{CaseResult, Manifest, Report, Stage, Status};

const DEFAULT_MANIFEST: &str = "fcc/checks/Inputs/fp/cases.toml";

#[derive(clap::Subcommand)]
pub enum Task {
    /// Compare saved observations with cumulative stage requirements.
    Check {
        /// Highest cumulative stage to check.
        #[arg(long)]
        stage: Stage,
        /// GCC reference report to compare.
        #[arg(long)]
        reference: PathBuf,
        /// JSON report to write.
        #[arg(long)]
        output: PathBuf,
        /// Run one stable case ID.
        #[arg(long)]
        case: Option<String>,
        /// Use a non-default case manifest.
        #[arg(long)]
        manifest: Option<PathBuf>,
    },
    /// Summarize a saved report without rerunning any compiler.
    Report {
        /// JSON report to summarize.
        input: PathBuf,
    },
}

pub fn run(root: &Path, task: Task) -> anyhow::Result<()> {
    match task {
        Task::Check {
            stage,
            reference,
            output,
            case,
            manifest,
        } => check(
            root,
            stage,
            &reference,
            &output,
            case.as_deref(),
            manifest.as_deref(),
        ),
        Task::Report { input } => report(&input),
    }
}

fn check(
    root: &Path,
    stage: Stage,
    reference_path: &Path,
    output_path: &Path,
    case_filter: Option<&str>,
    manifest_path: Option<&Path>,
) -> anyhow::Result<()> {
    let manifest_path = manifest_path
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join(DEFAULT_MANIFEST));
    let manifest: Manifest = toml::from_str(
        &fs::read_to_string(&manifest_path).with_context(|| manifest_path.display().to_string())?,
    )
    .with_context(|| manifest_path.display().to_string())?;
    anyhow::ensure!(manifest.schema_version == 1, "unsupported manifest schema version");
    let ids = manifest
        .cases
        .iter()
        .map(|case| case.id.as_str())
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(ids.len() == manifest.cases.len(), "duplicate case ID");

    let selected = manifest
        .cases
        .iter()
        .filter(|case| case.stage <= stage)
        .filter(|case| case_filter.is_none_or(|id| case.id == id))
        .collect::<Vec<_>>();
    anyhow::ensure!(!selected.is_empty(), "no cases selected");

    let reference: Report = serde_json::from_str(
        &fs::read_to_string(reference_path)
            .with_context(|| reference_path.display().to_string())?,
    )
    .with_context(|| reference_path.display().to_string())?;
    anyhow::ensure!(reference.schema_version == 1, "unsupported report schema version");
    anyhow::ensure!(
        reference.profile == manifest.reference.profile,
        "reference profile mismatch"
    );
    let by_id = reference
        .results
        .iter()
        .map(|result| (result.case_id.as_str(), result))
        .collect::<BTreeMap<_, _>>();

    let mut results = Vec::with_capacity(selected.len());
    for case in selected {
        let Some(reference_result) = by_id.get(case.id.as_str()) else {
            results.push(CaseResult {
                case_id: case.id.clone(),
                stage: case.stage,
                compiler: model::CompilerIdentity {
                    version: manifest.reference.compiler_version.clone(),
                    executable: String::new(),
                },
                source_digest: String::new(),
                commands: Vec::new(),
                exit_status: None,
                observation: None,
                resolved_policy: None,
                artifacts: None,
                status: Status::MissingInfrastructure,
                detail: "case is absent from the reference report".into(),
            });
            continue;
        };
        let comparison = case.expectation.compare(reference_result.observation.as_ref());
        let mut result = (*reference_result).clone();
        result.stage = case.stage;
        match comparison {
            Ok(()) => {
                result.status = Status::Pass;
                result.detail = format!("matches {} {}", case.oracle.identity, case.oracle.version);
            }
            Err(detail) => {
                result.status = Status::Fail;
                result.detail = detail;
            }
        }
        results.push(result);
    }

    let checked = Report {
        schema_version: 1,
        profile: reference.profile,
        generated_at_unix_seconds: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        host: reference.host,
        results,
    };
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(output_path, serde_json::to_vec_pretty(&checked)?)?;
    anyhow::ensure!(
        checked.results.iter().all(|result| result.status == Status::Pass),
        "floating-point comparison failed; report written to {}",
        output_path.display()
    );
    Ok(())
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
