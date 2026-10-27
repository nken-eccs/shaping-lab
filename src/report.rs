use crate::{
    Case, LoadedCorpus, PlanMode,
    corpus::sha256,
    engine::{Backend, Glyph, harfbuzz_version, shape_once},
};
use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

fn command(program: &str, args: &[&str]) -> Option<String> {
    let result = Command::new(program).args(args).output().ok()?;
    result
        .status
        .success()
        .then(|| String::from_utf8_lossy(&result.stdout).trim().to_owned())
}

pub fn metadata(corpus: &LoadedCorpus) -> Value {
    let cpu = if cfg!(target_os = "macos") {
        command("sysctl", &["-n", "machdep.cpu.brand_string"])
    } else {
        fs::read_to_string("/proc/cpuinfo").ok().and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .map(str::to_owned)
        })
    };
    json!({
        "schema_version": 1,
        "unix_time": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
        "build": {
            "rustc": env!("SHAPING_RUSTC"), "target": env!("SHAPING_TARGET"),
            "opt_level": env!("SHAPING_OPT_LEVEL"), "rustflags": env!("SHAPING_RUSTFLAGS"),
            "cxx": env!("SHAPING_CXX"), "cxxflags": env!("SHAPING_CXXFLAGS"),
            "cxx_version": env!("SHAPING_CXX_VERSION"),
            "harfbuzz_include": env!("SHAPING_HB_INCLUDE"),
            "source_sha256": env!("SHAPING_CODE_SHA256"),
            "harfbuzz_source_sha256": env!("SHAPING_HB_SOURCE_SHA256"),
            "cargo_lock_sha256": env!("SHAPING_LOCK_SHA256"),
            "trace": cfg!(feature = "trace"),

            "rust_debug_assertions": cfg!(debug_assertions),
            "harfbuzz_assertions": crate::engine::harfbuzz_assertions_enabled(),
        },
        "backends": { "harfbuzz": harfbuzz_version(), "local": "shaping-core 0.1.0", "optimized": "shaping-core with shared plans and metric caches" },
        "host": { "os": command("uname", &["-a"]), "cpu": cpu, "logical_cpus": std::thread::available_parallelism().ok().map(|n| n.get()) },
        "checkout": { "commit": command("git", &["rev-parse", "HEAD"]), "status": command("git", &["status", "--porcelain"]) },
        "corpus": { "path": corpus.path, "sha256": corpus.sha256, "spec": corpus.spec },
        "fonts": corpus.fonts.iter().map(|(name, font)| (name.clone(), json!({"sha256":font.sha256,"index":font.index,"bytes":font.bytes.len()}))).collect::<serde_json::Map<_,_>>(),
    })
}

pub fn write_json(path: Option<&Path>, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    if let Some(path) = path {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, bytes).with_context(|| format!("Cannot write {}", path.display()))?;
    } else {
        std::io::stdout().lock().write_all(&bytes)?;
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct Difference {
    pub run: usize,
    pub glyph: usize,
    pub left: Option<Glyph>,
    pub right: Option<Glyph>,
}

pub fn diff(left: &[Vec<Glyph>], right: &[Vec<Glyph>]) -> Vec<Difference> {
    let mut differences = Vec::new();
    for run in 0..left.len().max(right.len()) {
        let l = left.get(run).map(Vec::as_slice).unwrap_or_default();
        let r = right.get(run).map(Vec::as_slice).unwrap_or_default();
        for index in 0..l.len().max(r.len()) {
            if l.get(index) != r.get(index) {
                differences.push(Difference {
                    run,
                    glyph: index,
                    left: l.get(index).cloned(),
                    right: r.get(index).cloned(),
                });
            }
        }
    }
    differences
}

pub fn compare(
    corpus: &LoadedCorpus,
    cases: &[&Case],
    left: Backend,
    right: Backend,
    plan: PlanMode,
) -> Result<(Value, bool)> {
    let mut reports = Vec::new();
    let mut equal = true;
    for case in cases {
        let l = shape_once(left, corpus, case, plan)?;
        let r = shape_once(right, corpus, case, plan)?;
        let differences = diff(&l, &r);
        let matches = l == r;
        equal &= matches;
        reports.push(
            json!({ "case":case.id, "equal":matches, "differences":differences, "missing_glyphs": {
                "left": l.iter().flatten().filter(|g|g.id == 0).count(),
                "right": r.iter().flatten().filter(|g|g.id == 0).count()
            }}),
        );
    }
    Ok((
        json!({"metadata":metadata(corpus),"left":left,"right":right,"plan":plan,"equal":equal,"cases":reports}),
        equal,
    ))
}

pub fn input_fingerprint(corpus: &LoadedCorpus) -> Result<String> {
    Ok(sha256(&serde_json::to_vec(
        &json!({"spec":corpus.spec,"fonts":corpus.fonts.iter().map(|(k,v)|(k,&v.sha256)).collect::<std::collections::BTreeMap<_,_>>()}),
    )?))
}
