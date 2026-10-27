use anyhow::{Result, ensure};
use clap::{Parser, Subcommand};
use serde_json::json;
use shaping_lab::{
    Backend, FontSet, LoadedCorpus, Mode, PlanMode, Session, engine::shape_once, report,
};
use std::{
    hint::black_box,
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Parser)]
#[command(about = "Reproducible shaping experiments", version)]
struct Args {
    #[arg(long, global = true, default_value = "corpus/default.toml")]
    corpus: PathBuf,
    #[arg(long, global = true)]
    case: Option<String>,
    #[arg(long, global = true, value_enum, default_value = "cached")]
    plan: PlanMode,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List cases and input sizes
    List,
    /// Export positioned glyphs as JSON
    Shape {
        #[arg(long, value_enum, default_value = "optimized")]
        backend: Backend,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Compare glyphs exactly (exit 2 on differences)
    Compare {
        #[arg(long, value_enum, default_value = "harfbuzz")]
        left: Backend,
        #[arg(long, value_enum, default_value = "optimized")]
        right: Backend,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Print build and input provenance
    Doctor {
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Run a quiet workload for an external profiler
    Profile {
        #[arg(long, value_enum)]
        backend: Backend,
        #[arg(long, value_enum, default_value = "reuse")]
        mode: Mode,
        #[arg(long, default_value_t = 20.0)]
        seconds: f64,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Print shaping events to stderr and final glyphs to stdout
    Trace {
        #[arg(long, value_enum, default_value = "harfbuzz")]
        backend: Backend,
    },
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("Error: {error:#}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<i32> {
    let args = Args::parse();
    let corpus = LoadedCorpus::load(&args.corpus)?;
    let cases = corpus.cases(args.case.as_deref())?;
    match args.command {
        Command::List => {
            println!(
                "{:<25} {:>5} {:>7} {:>7}  DESCRIPTION",
                "CASE", "RUNS", "BYTES", "CHARS"
            );
            for case in cases {
                println!(
                    "{:<25} {:>5} {:>7} {:>7}  {}",
                    case.id,
                    case.runs.len(),
                    case.bytes(),
                    case.chars(),
                    case.description
                );
            }
        }
        Command::Doctor { out } => report::write_json(out.as_deref(), &report::metadata(&corpus))?,
        Command::Shape { backend, out } => {
            let shapes = cases
                .iter()
                .map(|case| {
                    let glyphs = shape_once(backend, &corpus, case, args.plan)?;
                    Ok(json!({"case":case.id,"runs":glyphs}))
                })
                .collect::<Result<Vec<_>>>()?;
            report::write_json(
                out.as_deref(),
                &json!({"metadata":report::metadata(&corpus),"backend":backend,"plan":args.plan,"cases":shapes}),
            )?;
        }
        Command::Compare { left, right, out } => {
            let (value, equal) = report::compare(&corpus, &cases, left, right, args.plan)?;
            if out.is_some() {
                eprintln!(
                    "{}: {} cases",
                    if equal { "MATCH" } else { "DIFFERENT" },
                    cases.len()
                );
            }
            report::write_json(out.as_deref(), &value)?;
            if !equal {
                return Ok(2);
            }
        }
        Command::Profile {
            backend,
            mode,
            seconds,
            out,
        } => {
            ensure!(args.case.is_some(), "profile requires --case");
            ensure!(
                seconds.is_finite() && seconds > 0.0 && seconds <= 86400.0,
                "seconds must be in (0, 86400]"
            );
            ensure!(
                !cfg!(feature = "trace"),
                "Use a build without the trace feature for profiling"
            );
            let case = cases[0];
            let fonts = FontSet::new(backend, &corpus, case)?;
            let mut session = Session::new(backend, &fonts, case, args.plan)?;
            for _ in 0..100 {
                black_box(session.shape(false)?);
            }
            let duration = Duration::from_secs_f64(seconds);
            let start = Instant::now();
            let mut iterations = 0_u64;
            while start.elapsed() < duration {
                for _ in 0..64 {
                    if mode == Mode::FreshFont {
                        let fonts = FontSet::new(backend, &corpus, case)?;
                        let mut session = Session::new(backend, &fonts, case, args.plan)?;
                        black_box(session.shape(false)?);
                    } else {
                        black_box(session.shape(mode == Mode::FreshBuffer)?);
                    }
                    iterations += 1;
                }
            }
            let elapsed = start.elapsed();
            report::write_json(
                out.as_deref(),
                &json!({"metadata":report::metadata(&corpus),"backend":backend,"plan":args.plan,"case":case.id,"mode":mode,"iterations":iterations,"elapsed_seconds":elapsed.as_secs_f64()}),
            )?;
        }
        Command::Trace { backend } => {
            ensure!(args.case.is_some(), "trace requires --case");
            let case = cases[0];
            let fonts = FontSet::new(backend, &corpus, case)?;
            let mut session = Session::new(backend, &fonts, case, args.plan)?;
            session.trace()?;
            session.shape(false)?;
            report::write_json(
                None,
                &json!({"case":case.id,"backend":backend,"runs":session.glyphs()}),
            )?;
        }
    }
    Ok(0)
}
