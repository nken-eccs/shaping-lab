use anyhow::{Context, Result, ensure};
use clap::ValueEnum;
use criterion::{BenchmarkId, Criterion, SamplingMode, Throughput};
use serde_json::json;
use shaping_lab::{Backend, FontSet, LoadedCorpus, Mode, PlanMode, Session, report};
use std::time::{Duration, Instant};
use std::{env, hint::black_box, path::PathBuf};

fn values<T: ValueEnum>(key: &str, default: &str) -> Result<Vec<T>> {
    env::var(key)
        .unwrap_or_else(|_| default.to_owned())
        .split(',')
        .map(|s| T::from_str(s, false).map_err(anyhow::Error::msg))
        .collect()
}

fn shaping() -> Result<()> {
    ensure!(
        !cfg!(feature = "trace"),
        "Benchmark without the trace feature"
    );
    ensure!(
        !shaping_lab::engine::harfbuzz_assertions_enabled() && !cfg!(debug_assertions),
        "Benchmark with Rust and HarfBuzz debug assertions disabled"
    );
    let corpus = LoadedCorpus::load(
        env::var("SHAPING_CORPUS").unwrap_or_else(|_| "corpus/default.toml".into()),
    )?;
    let filter = env::var("SHAPING_CASE").ok();
    let cases = corpus.cases(filter.as_deref())?;
    let backends = values::<Backend>("SHAPING_BACKENDS", "harfbuzz,local,optimized")?;
    let modes = values::<Mode>("SHAPING_MODES", "reuse")?;
    let plan = values::<PlanMode>("SHAPING_PLAN", "cached")?;
    ensure!(plan.len() == 1, "Specify exactly one plan policy");
    let plan = plan[0];
    let directory =
        PathBuf::from(env::var("SHAPING_OUTPUT").unwrap_or_else(|_| "target/shaping-bench".into()));
    let fingerprint = report::input_fingerprint(&corpus)?;
    let selection = json!({"cases":cases.iter().map(|c|&c.id).collect::<Vec<_>>(),"backends":backends,"modes":modes,"plan":plan,"input_sha256":fingerprint});
    if let Ok(baseline) = env::var("SHAPING_BASELINE_METADATA") {
        let previous: serde_json::Value = serde_json::from_slice(&std::fs::read(baseline)?)?;
        ensure!(
            previous["selection"] == selection,
            "Baseline inputs or benchmark selection differ"
        );
        let current = report::metadata(&corpus);
        for field in [
            "target",
            "opt_level",
            "rustflags",
            "cxx",
            "trace",
            "rust_debug_assertions",
            "harfbuzz_assertions",
        ] {
            ensure!(
                previous["metadata"]["build"][field] == current["build"][field],
                "Baseline build setting differs: {field}"
            );
        }
        let flags = |metadata: &serde_json::Value| {
            let build = &metadata["build"];
            build["cxxflags"].as_str().unwrap_or_default().replace(
                build["harfbuzz_include"]
                    .as_str()
                    .unwrap_or("vendor/harfbuzz/src"),
                "<harfbuzz-include>",
            )
        };
        ensure!(
            flags(&previous["metadata"]) == flags(&current),
            "Baseline C++ options differ"
        );
    }
    report::write_json(
        Some(&directory.join("metadata.json")),
        &json!({"selection":selection,"metadata":report::metadata(&corpus)}),
    )?;
    let mut criterion = Criterion::default()
        .output_directory(&directory.join("criterion"))
        .sample_size(50)
        .warm_up_time(std::time::Duration::from_secs(1))
        .measurement_time(std::time::Duration::from_secs(3))
        .configure_from_args();
    for case in cases {
        let mut group = criterion.benchmark_group(&case.id);
        if case.bytes() > 0 {
            group.throughput(Throughput::Bytes(case.bytes() as u64));
        }
        for mode in &modes {
            for backend in &backends {
                let id = BenchmarkId::new(format!("{mode}-{plan}"), backend);
                let fonts = FontSet::new(*backend, &corpus, case)
                    .with_context(|| format!("{} / {backend}", case.id))?;
                let mut session = Session::new(*backend, &fonts, case, plan)?;
                session.shape(false)?;
                group.bench_function(id, |b| {
                    if *mode == Mode::FreshFont {
                        b.iter(|| {
                            let fonts = FontSet::new(*backend, black_box(&corpus), black_box(case))
                                .unwrap();
                            let mut session = Session::new(*backend, &fonts, case, plan).unwrap();
                            black_box(session.shape(false).unwrap());
                        });
                    } else {
                        b.iter(|| {
                            black_box(
                                session
                                    .shape(black_box(*mode == Mode::FreshBuffer))
                                    .unwrap(),
                            )
                        });
                    }
                });
            }
        }
        group.finish();
    }
    criterion.final_summary();
    Ok(())
}

fn stages() -> Result<()> {
    ensure!(
        !cfg!(feature = "trace"),
        "Benchmark without the trace feature"
    );
    ensure!(
        !cfg!(debug_assertions) && !shaping_lab::engine::harfbuzz_assertions_enabled(),
        "Benchmark with debug assertions disabled"
    );
    let corpus = LoadedCorpus::load(
        env::var("SHAPING_CORPUS").unwrap_or_else(|_| "corpus/default.toml".into()),
    )?;
    let filter = env::var("SHAPING_CASE").ok();
    let mut cases = corpus
        .cases(filter.as_deref())?
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    if env::var("SHAPING_SPLIT_RUNS").as_deref() == Ok("1") {
        cases = cases
            .into_iter()
            .flat_map(|case| {
                case.runs
                    .iter()
                    .enumerate()
                    .map(|(i, run)| {
                        let mut single = case.clone();
                        single.id = format!("{}.run{i}", case.id);
                        single.runs = vec![run.clone()];
                        single
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
    }
    let backends = env::var("SHAPING_BACKENDS")
        .unwrap_or_else(|_| "harfbuzz,local,optimized".into())
        .split(',')
        .map(|s| Backend::from_str(s, false).map_err(anyhow::Error::msg))
        .collect::<Result<Vec<_>>>()?;
    let batch: usize = env::var("SHAPING_BATCH_SIZE")
        .unwrap_or_else(|_| "64".into())
        .parse()?;
    ensure!(
        (1..=1024).contains(&batch),
        "batch size must be in [1, 1024]"
    );
    let directory =
        PathBuf::from(env::var("SHAPING_OUTPUT").unwrap_or_else(|_| "target/stages".into()));
    report::write_json(
        Some(&directory.join("metadata.json")),
        &json!({
            "metadata":report::metadata(&corpus), "cases":cases, "backends":backends,
            "suite":"stages", "batch_size":batch, "plan":"cached", "mode":"reuse"
        }),
    )?;
    let mut criterion = Criterion::default()
        .output_directory(&directory.join("criterion"))
        .sample_size(50)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3))
        .configure_from_args();
    for case in &cases {
        let mut group = criterion.benchmark_group(&case.id);
        group.sampling_mode(SamplingMode::Flat);
        for backend in &backends {
            let fonts = FontSet::new(*backend, &corpus, case)?;
            let mut sessions = (0..batch)
                .map(|_| Session::new(*backend, &fonts, case, PlanMode::Cached))
                .collect::<Result<Vec<_>>>()?;
            for session in &mut sessions {
                session.shape(false)?;
            }
            for stage in ["prepare", "plan", "shape", "total", "font-object"] {
                group.bench_function(
                    BenchmarkId::new(format!("{stage}-b{batch}"), backend),
                    |b| {
                        b.iter_custom(|iterations| {
                            let mut remaining = iterations;
                            let mut elapsed = Duration::ZERO;
                            while remaining > 0 {
                                let count = remaining.min(batch as u64) as usize;
                                let active = &mut sessions[..count];
                                if stage == "shape" {
                                    for session in &mut *active {
                                        session.prepare_buffers().unwrap();
                                    }
                                }
                                let start = Instant::now();
                                for session in active {
                                    match stage {
                                        "prepare" => {
                                            session.prepare_buffers().unwrap();
                                            black_box(&*session);
                                        }
                                        "plan" => session.measure_plans(),
                                        "shape" => {
                                            black_box(session.execute().unwrap());
                                        }
                                        "total" => {
                                            black_box(session.shape(false).unwrap());
                                        }
                                        "font-object" => drop(black_box(
                                            FontSet::new(*backend, &corpus, case).unwrap(),
                                        )),
                                        _ => unreachable!(),
                                    }
                                }
                                elapsed += start.elapsed();
                                remaining -= count as u64;
                            }
                            elapsed
                        });
                    },
                );
            }
        }
        group.finish();
    }
    criterion.final_summary();
    Ok(())
}

fn stream() -> Result<()> {
    use shaping_lab::shaper::{CacheOptions, Context, Font, Shaper};
    ensure!(
        !cfg!(feature = "trace") && !cfg!(debug_assertions),
        "Benchmark in release mode without tracing"
    );
    let corpus = LoadedCorpus::load(
        env::var("SHAPING_CORPUS").unwrap_or_else(|_| "corpus/default.toml".into()),
    )?;
    let filter = env::var("SHAPING_CASE").ok();
    let cases = corpus.cases(filter.as_deref())?;
    let directory =
        PathBuf::from(env::var("SHAPING_OUTPUT").unwrap_or_else(|_| "target/stream".into()));
    report::write_json(
        Some(&directory.join("metadata.json")),
        &json!({
            "metadata":report::metadata(&corpus), "cases":cases, "suite":"stream",
            "texts":"full, prefix, suffix", "passes_per_iteration":3,
            "variants":["metrics-off", "optimized", "prepare-each", "rebuild-plan"]
        }),
    )?;
    let mut criterion = Criterion::default()
        .output_directory(&directory.join("criterion"))
        .sample_size(50)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3))
        .configure_from_args();
    for case in cases {
        let mut group = criterion.benchmark_group(&case.id);
        let properties = case
            .runs
            .iter()
            .map(|run| run.properties())
            .collect::<Result<Vec<_>>>()?;
        let inputs = case
            .runs
            .iter()
            .map(|run| {
                let split = run
                    .text
                    .char_indices()
                    .nth(run.text.chars().count() / 2)
                    .map_or(0, |(i, _)| i);
                [
                    (
                        run.text.clone(),
                        run.pre_context.clone(),
                        run.post_context.clone(),
                    ),
                    (
                        run.text[..split].into(),
                        run.pre_context.clone(),
                        format!("{}{}", &run.text[split..], run.post_context),
                    ),
                    (
                        run.text[split..].into(),
                        format!("{}{}", run.pre_context, &run.text[..split]),
                        run.post_context.clone(),
                    ),
                ]
            })
            .collect::<Vec<_>>();
        for variant in ["metrics-off", "optimized", "prepare-each", "rebuild-plan"] {
            let options = CacheOptions {
                plans_per_font: if variant == "rebuild-plan" { 0 } else { 16 },
                horizontal_advances: variant != "metrics-off",
                vertical_origins: variant != "metrics-off",
            };
            let mut fonts = Vec::new();
            let mut indices = Vec::new();
            for (index, run) in case.runs.iter().enumerate() {
                let previous = case.runs[..index]
                    .iter()
                    .position(|other| run.font == other.font && run.variations == other.variations);
                let font = if let Some(previous) = previous {
                    indices[previous]
                } else {
                    fonts.push(Font::with_options(
                        corpus.fonts[&run.font].instantiate(&run.variations)?,
                        options,
                    ));
                    fonts.len() - 1
                };
                indices.push(font);
            }
            let prepared = indices
                .iter()
                .zip(&properties)
                .map(|(index, properties)| fonts[*index].prepare(properties.clone()))
                .collect::<Result<Vec<_>>>()?;
            let mut shaper = Shaper::default();
            let mut cycle = || {
                for phase in 0..3 {
                    for (index, inputs) in inputs.iter().enumerate() {
                        let (text, before, after) = &inputs[phase];
                        let temporary;
                        let run = if matches!(variant, "prepare-each" | "rebuild-plan") {
                            temporary = fonts[indices[index]]
                                .prepare(black_box(properties[index].clone()))
                                .unwrap();
                            &temporary
                        } else {
                            &prepared[index]
                        };
                        black_box(
                            shaper
                                .shape(run, black_box(text), Context { before, after })
                                .unwrap(),
                        );
                    }
                }
            };
            cycle();
            group.bench_function(variant, |b| b.iter(&mut cycle));
        }
        group.finish();
    }
    criterion.final_summary();
    Ok(())
}

fn batch() -> Result<()> {
    use shaping_lab::shaper::{BatchShaper, RunConfig, RunInput};
    use std::sync::Arc;
    ensure!(
        !cfg!(feature = "trace") && !cfg!(debug_assertions),
        "Benchmark in release mode without tracing"
    );
    let corpus = LoadedCorpus::load(
        env::var("SHAPING_CORPUS").unwrap_or_else(|_| "corpus/default.toml".into()),
    )?;
    let filter = env::var("SHAPING_CASE").ok();
    let cases = corpus.cases(filter.as_deref())?;
    let numbers = |name, default: &str| -> Result<Vec<usize>> {
        env::var(name)
            .unwrap_or_else(|_| default.into())
            .split(',')
            .map(|value| value.parse().map_err(anyhow::Error::from))
            .collect()
    };
    let sizes = numbers("SHAPING_BATCH_SIZES", "1,32,128,512,2048")?;
    let workers = numbers("SHAPING_WORKERS", "1,2,4,8")?;
    let minimum: usize = env::var("SHAPING_MIN_RUNS_PER_WORKER")
        .unwrap_or_else(|_| "32".into())
        .parse()?;
    ensure!(
        sizes.iter().all(|n| (1..=65536).contains(n)),
        "Invalid batch size"
    );
    ensure!(
        workers.iter().all(|n| (1..=64).contains(n)),
        "Invalid worker count"
    );
    let directory =
        PathBuf::from(env::var("SHAPING_OUTPUT").unwrap_or_else(|_| "target/batch".into()));
    report::write_json(
        Some(&directory.join("metadata.json")),
        &json!({
            "metadata":report::metadata(&corpus), "cases":cases, "suite":"batch",
            "batch_sizes":sizes, "workers":workers, "min_runs_per_worker":minimum,
            "texts":"full, prefix, suffix", "output_copy":true, "input_order":true
        }),
    )?;
    let mut criterion = Criterion::default()
        .output_directory(&directory.join("criterion"))
        .warm_up_time(Duration::from_secs(1))
        .configure_from_args();
    for case in cases {
        let mut group = criterion.benchmark_group(&case.id);
        let mut fonts = Vec::new();
        let mut indices = Vec::new();
        let mut configs = Vec::new();
        let mut templates = Vec::new();
        for (index, run) in case.runs.iter().enumerate() {
            let previous = case.runs[..index]
                .iter()
                .position(|other| other.font == run.font && other.variations == run.variations);
            let font = if let Some(previous) = previous {
                indices[previous]
            } else {
                fonts.push(corpus.fonts[&run.font].instantiate(&run.variations)?);
                fonts.len() - 1
            };
            indices.push(font);
            configs.push(RunConfig {
                font,
                properties: run.properties()?,
            });
            let text: Arc<str> =
                format!("{}{}{}", run.pre_context, run.text, run.post_context).into();
            let split = run
                .text
                .char_indices()
                .nth(run.text.chars().count() / 2)
                .map_or(0, |(i, _)| i);
            let start = run.pre_context.len();
            for range in [0..run.text.len(), 0..split, split..run.text.len()] {
                templates.push(RunInput {
                    config: index,
                    text: text.clone(),
                    range: start + range.start..start + range.end,
                });
            }
        }
        for count in &sizes {
            let inputs: Arc<[RunInput]> = (0..*count)
                .map(|index| templates[index % templates.len()].clone())
                .collect();
            group.throughput(Throughput::Elements(*count as u64));
            for threads in &workers {
                let mut shaper = BatchShaper::new(fonts.clone(), configs.clone(), *threads)?;
                shaper.set_min_runs_per_worker(minimum)?;
                shaper.shape(inputs.clone())?;
                group.bench_function(
                    BenchmarkId::new(format!("runs{count}"), format!("workers{threads}")),
                    |b| {
                        b.iter(|| {
                            black_box(shaper.shape(black_box(inputs.clone())).unwrap());
                        });
                    },
                );
            }
        }
        group.finish();
    }
    criterion.final_summary();
    Ok(())
}

fn main() {
    let result = match env::var("SHAPING_SUITE").as_deref() {
        Ok("stages") => stages(),
        Ok("stream") => stream(),
        Ok("batch") => batch(),
        Ok("shaping") | Err(_) => shaping(),
        Ok(other) => Err(anyhow::anyhow!("Unknown benchmark suite: {other}")),
    };
    if let Err(error) = result {
        eprintln!("Error: {error:#}");
        std::process::exit(1);
    }
}
