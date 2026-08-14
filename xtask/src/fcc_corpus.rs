use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use xshell::{cmd, Shell};

const CORPUS_DIRS: [&str; 2] = ["fcc/checks", "fcc/tests"];

pub enum Mode {
    Report,
    Baseline(PathBuf),
    Diff(PathBuf),
    Determinism,
}

struct Compiled {
    name: String,
    asm: Option<String>,
}

pub fn run(sh: &Shell, root: &Path, mode: Mode) -> anyhow::Result<()> {
    cmd!(sh, "cargo build -j4 -p fcc --bin fcc").run()?;
    let fcc = root.join("target/debug/fcc");
    let files = collect(root)?;
    println!("fcc corpus: {} files", files.len());

    let results = compile_all(&fcc, root, &files);
    let compiled = results.iter().filter(|entry| entry.asm.is_some()).count();
    println!("compiles: {compiled}/{}", results.len());

    match mode {
        Mode::Report => report(&results),
        Mode::Baseline(dir) => capture(&dir, &results)?,
        Mode::Diff(dir) => diff(&dir, &results)?,
        Mode::Determinism => determinism(&fcc, root, &files, &results),
    }
    Ok(())
}

fn report(results: &[Compiled]) {
    let total: usize = results
        .iter()
        .filter_map(|e| e.asm.as_deref())
        .map(instructions)
        .sum();
    println!("instructions: {total}");
}

fn capture(dir: &Path, results: &[Compiled]) -> anyhow::Result<()> {
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir)?;
    let mut total = 0;
    for entry in results {
        let Some(asm) = &entry.asm else { continue };
        fs::write(dir.join(&entry.name), asm)?;
        total += instructions(asm);
    }
    println!(
        "baseline captured in {}: {total} instructions",
        dir.display()
    );
    Ok(())
}

fn diff(dir: &Path, results: &[Compiled]) -> anyhow::Result<()> {
    let (mut identical, mut changed, mut worse, mut better, mut net) = (0, 0, 0, 0, 0i64);
    for entry in results {
        let path = dir.join(&entry.name);
        let base = fs::read_to_string(&path).ok();
        match (&base, &entry.asm) {
            (None, None) => {}
            (None, Some(_)) => println!("{}: NEW (was not in baseline)", entry.name),
            (Some(_), None) => println!("{}: LOST (no longer compiles)", entry.name),
            (Some(base), Some(asm)) if base == asm => identical += 1,
            (Some(base), Some(asm)) => {
                changed += 1;
                let delta = instructions(asm) as i64 - instructions(base) as i64;
                net += delta;
                if delta > 0 {
                    worse += 1;
                } else if delta < 0 {
                    better += 1;
                }
                println!("{}: {delta:+}", entry.name);
            }
        }
    }
    println!(
        "identical {identical}, changed {changed} ({worse} worse, {better} better), net {net:+}"
    );
    Ok(())
}

fn determinism(fcc: &Path, root: &Path, files: &[PathBuf], first: &[Compiled]) {
    let second = compile_all(fcc, root, files);
    let diffs = first
        .iter()
        .zip(&second)
        .filter(|(left, right)| left.asm != right.asm)
        .map(|(left, _)| left.name.clone())
        .collect::<Vec<_>>();
    for name in &diffs {
        println!("{name}: NONDETERMINISTIC");
    }
    println!("determinism: {} diffs / {} files", diffs.len(), first.len());
}

fn instructions(asm: &str) -> usize {
    asm.lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            line.len() != trimmed.len() && trimmed.starts_with(|c: char| c.is_ascii_lowercase())
        })
        .count()
}

fn collect(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for dir in CORPUS_DIRS {
        walk(&root.join(dir), &mut files)?;
    }
    files.sort();
    Ok(files)
}

fn walk(dir: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            walk(&path, files)?;
        } else if path.extension().is_some_and(|extension| extension == "c") {
            files.push(path);
        }
    }
    Ok(())
}

fn compile_all(fcc: &Path, root: &Path, files: &[PathBuf]) -> Vec<Compiled> {
    let next = Arc::new(AtomicUsize::new(0));
    let results = Arc::new(Mutex::new(Vec::with_capacity(files.len())));
    let workers = std::thread::available_parallelism().map_or(1, usize::from);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let next = Arc::clone(&next);
            let results = Arc::clone(&results);
            scope.spawn(move || loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(file) = files.get(index) else { break };
                let output = Command::new(fcc)
                    .args(["compile", "--stage", "asm", "--march", "x86_64", "-o", "-"])
                    .arg(file)
                    .output();
                let asm = output
                    .ok()
                    .filter(|out| out.status.success())
                    .and_then(|out| String::from_utf8(out.stdout).ok());
                results.lock().unwrap().push((
                    index,
                    Compiled {
                        name: name(root, file),
                        asm,
                    },
                ));
            });
        }
    });
    let mut results = Arc::into_inner(results).unwrap().into_inner().unwrap();
    results.sort_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, entry)| entry).collect()
}

fn name(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace(['/', '\\'], "_")
}

#[cfg(test)]
mod tests {
    use super::instructions;

    #[test]
    fn only_indented_mnemonics_are_instructions() {
        let asm = ".global fib\nfib:\n\tpush rbx\n\tmov rax, rsp\n.Lbb1:\n\t.quad 0\n";
        assert_eq!(instructions(asm), 2);
    }
}
