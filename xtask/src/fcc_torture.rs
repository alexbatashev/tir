use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use xshell::{cmd, Shell};

const GCC_REPOSITORY: &str = "https://github.com/gcc-mirror/gcc.git";
const GCC_REVISION: &str = "9aab80ddc5b2fa0eef80008e718067ab45f42c50";
const TORTURE_PATH: &str = "gcc/testsuite/gcc.c-torture";

pub fn run(sh: &Shell, root: &Path, bless: bool) -> anyhow::Result<()> {
    let checkout = root.join("target/test-suites/gcc");
    fetch_gcc(sh, &checkout)?;

    cmd!(sh, "cargo build -p fcc --bin fcc").run()?;
    let fcc = root.join("target/debug/fcc");
    let corpus = checkout.join(TORTURE_PATH);
    let mut files = Vec::new();
    collect_c_files(&corpus.join("compile"), &mut files)?;
    collect_c_files(&corpus.join("execute"), &mut files)?;
    files.sort();

    let results = run_parser(&fcc, &corpus, files)?;
    let allowlist_path = root.join("fcc/tests/gcc-torture-known-failures.txt");
    if bless {
        let failures = results
            .iter()
            .filter_map(|(path, passed)| (!passed).then_some(path.as_str()))
            .collect::<Vec<_>>();
        let contents = if failures.is_empty() {
            String::new()
        } else {
            format!("{}\n", failures.join("\n"))
        };
        fs::write(&allowlist_path, contents)?;
        println!("recorded {} known GCC torture failures", failures.len());
        return Ok(());
    }

    let expected = parse_allowlist(&fs::read_to_string(&allowlist_path)?)?;
    let classification = classify_results(&expected, &results);
    let passed = results.iter().filter(|(_, passed)| *passed).count();
    println!(
        "GCC torture parser: {passed}/{} passed, {} expected failures",
        results.len(),
        expected.len()
    );
    print_paths("unexpected failures", &classification.unexpected_failures);
    print_paths("stale failures", &classification.stale_failures);
    print_paths("missing allowlist entries", &classification.missing_entries);
    if !classification.unexpected_failures.is_empty()
        || !classification.stale_failures.is_empty()
        || !classification.missing_entries.is_empty()
    {
        anyhow::bail!("GCC torture parser baseline changed");
    }
    Ok(())
}

fn fetch_gcc(sh: &Shell, checkout: &Path) -> anyhow::Result<()> {
    if !checkout.join(".git").is_dir() {
        fs::create_dir_all(checkout)?;
        cmd!(sh, "git -C {checkout} init").run()?;
        cmd!(sh, "git -C {checkout} remote add origin {GCC_REPOSITORY}").run()?;
        cmd!(sh, "git -C {checkout} sparse-checkout set {TORTURE_PATH}").run()?;
    }
    cmd!(
        sh,
        "git -C {checkout} fetch --depth 1 --filter=blob:none origin {GCC_REVISION}"
    )
    .run()?;
    cmd!(sh, "git -C {checkout} checkout --detach FETCH_HEAD").run()?;
    Ok(())
}

fn collect_c_files(directory: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_c_files(&path, files)?;
        } else if path.extension().is_some_and(|extension| extension == "c") {
            files.push(path);
        }
    }
    Ok(())
}

fn run_parser(
    fcc: &Path,
    corpus: &Path,
    files: Vec<PathBuf>,
) -> anyhow::Result<Vec<(String, bool)>> {
    let files = Arc::new(files);
    let next = Arc::new(AtomicUsize::new(0));
    let results = Arc::new(Mutex::new(Vec::with_capacity(files.len())));
    let workers = std::thread::available_parallelism().map_or(1, usize::from);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let files = Arc::clone(&files);
            let next = Arc::clone(&next);
            let results = Arc::clone(&results);
            scope.spawn(move || loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(file) = files.get(index) else {
                    break;
                };
                let passed = Command::new(fcc)
                    .args(["compile", "-std=gnu17", "--stage", "ast", "-o", "-"])
                    .arg(file)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|status| status.success());
                let path = file
                    .strip_prefix(corpus)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                results.lock().unwrap().push((path, passed));
            });
        }
    });
    let mut results = Arc::into_inner(results).unwrap().into_inner().unwrap();
    results.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(results)
}

fn print_paths(label: &str, paths: &[String]) {
    if paths.is_empty() {
        return;
    }
    eprintln!("{label}:");
    for path in paths {
        eprintln!("  {path}");
    }
}

struct Classification {
    unexpected_failures: Vec<String>,
    stale_failures: Vec<String>,
    missing_entries: Vec<String>,
}

fn classify_results(expected: &BTreeSet<String>, results: &[(String, bool)]) -> Classification {
    let mut unexpected_failures = Vec::new();
    let mut stale_failures = Vec::new();
    let result_paths = results
        .iter()
        .map(|(path, _)| path.as_str())
        .collect::<BTreeSet<_>>();
    for (path, passed) in results {
        match (*passed, expected.contains(path)) {
            (false, false) => unexpected_failures.push(path.clone()),
            (true, true) => stale_failures.push(path.clone()),
            _ => {}
        }
    }
    Classification {
        unexpected_failures,
        stale_failures,
        missing_entries: expected
            .iter()
            .filter(|path| !result_paths.contains(path.as_str()))
            .cloned()
            .collect(),
    }
}

fn parse_allowlist(contents: &str) -> anyhow::Result<BTreeSet<String>> {
    let mut paths = BTreeSet::new();
    for line in contents.lines() {
        let path = line.trim();
        if path.is_empty() || path.starts_with('#') {
            continue;
        }
        if !paths.insert(path.to_string()) {
            anyhow::bail!("duplicate GCC torture allowlist entry: {path}");
        }
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{classify_results, parse_allowlist};

    #[test]
    fn unlisted_failure_is_a_regression() {
        let result = classify_results(&BTreeSet::new(), &[("compile/new.c".to_string(), false)]);
        assert_eq!(result.unexpected_failures, ["compile/new.c"]);
        assert!(result.stale_failures.is_empty());
    }

    #[test]
    fn listed_success_is_stale() {
        let expected = BTreeSet::from(["execute/fixed.c".to_string()]);
        let result = classify_results(&expected, &[("execute/fixed.c".to_string(), true)]);
        assert_eq!(result.stale_failures, ["execute/fixed.c"]);
        assert!(result.unexpected_failures.is_empty());
    }

    #[test]
    fn duplicate_allowlist_entry_is_rejected() {
        assert!(parse_allowlist("compile/a.c\ncompile/a.c\n").is_err());
    }

    #[test]
    fn missing_allowlist_path_is_reported() {
        let expected = BTreeSet::from(["compile/removed.c".to_string()]);
        let result = classify_results(&expected, &[]);
        assert_eq!(result.missing_entries, ["compile/removed.c"]);
    }
}
