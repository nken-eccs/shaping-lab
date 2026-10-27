mod harfbuzz;
mod hb_ffi;

use crate::corpus::{Case, LoadedCorpus};
use anyhow::Result;
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::{fmt, hint::black_box};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Backend {
    Harfbuzz,
    Local,
    #[value(alias = "experimental")]
    Optimized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Reuse,
    FreshBuffer,
    FreshFont,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlanMode {
    Cached,
    Rebuild,
    Native,
}

macro_rules! display_value {
    ($($ty:ty),*) => { $(impl fmt::Display for $ty {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.to_possible_value().unwrap().get_name()) }
    })* };
}
display_value!(Backend, Mode, PlanMode);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Glyph {
    pub id: u32,
    pub cluster: u32,
    pub x_advance: i32,
    pub y_advance: i32,
    pub x_offset: i32,
    pub y_offset: i32,
    pub flags: u32,
}

/// 設定済みランの処理 出力コピーは計測外
pub trait RunEngine {
    fn shape(&mut self, fresh_buffer: bool) -> Result<usize>;
    fn prepare_buffer(&mut self, fresh_buffer: bool) -> Result<()>;
    fn execute(&mut self) -> Result<usize>;
    fn measure_plan(&self);
    fn glyphs(&self) -> Vec<Glyph>;
    fn trace(&mut self) -> Result<()>;
}

pub enum FontSet {
    Harfbuzz(Vec<harfbuzz::Font>),
    Local(Vec<std::rc::Rc<crate::shaper::Font>>),
    Optimized(Vec<std::rc::Rc<crate::shaper::Font>>),
}

impl FontSet {
    pub fn new(backend: Backend, corpus: &LoadedCorpus, case: &Case) -> Result<Self> {
        match backend {
            Backend::Harfbuzz => Ok(Self::Harfbuzz(
                case.runs
                    .iter()
                    .map(|run| harfbuzz::Font::new(&corpus.fonts[&run.font], run))
                    .collect::<Result<_>>()?,
            )),
            Backend::Local | Backend::Optimized => {
                let optimized = backend == Backend::Optimized;
                let options = if optimized {
                    crate::shaper::CacheOptions::default()
                } else {
                    crate::shaper::CacheOptions {
                        plans_per_font: 0,
                        horizontal_advances: false,
                        vertical_origins: false,
                    }
                };
                let mut fonts = Vec::new();
                for (index, run) in case.runs.iter().enumerate() {
                    let previous = optimized
                        .then(|| {
                            case.runs[..index].iter().position(|other| {
                                let a = &corpus.fonts[&run.font];
                                let b = &corpus.fonts[&other.font];
                                a.sha256 == b.sha256
                                    && a.index == b.index
                                    && run.variations == other.variations
                            })
                        })
                        .flatten();
                    let font = if let Some(previous) = previous {
                        std::rc::Rc::clone(&fonts[previous])
                    } else {
                        std::rc::Rc::new(crate::shaper::Font::with_options(
                            corpus.fonts[&run.font].instantiate(&run.variations)?,
                            options,
                        ))
                    };
                    fonts.push(font);
                }
                Ok(if optimized {
                    Self::Optimized(fonts)
                } else {
                    Self::Local(fonts)
                })
            }
        }
    }
}

pub struct Session<'a> {
    runs: Vec<Box<dyn RunEngine + 'a>>,
}

impl<'a> Session<'a> {
    pub fn new(
        backend: Backend,
        fonts: &'a FontSet,
        case: &'a Case,
        plan: PlanMode,
    ) -> Result<Self> {
        let runs = case
            .runs
            .iter()
            .enumerate()
            .map(|(index, run)| -> Result<Box<dyn RunEngine + 'a>> {
                match (backend, fonts) {
                    (Backend::Harfbuzz, FontSet::Harfbuzz(fonts)) => {
                        Ok(Box::new(harfbuzz::Run::new(&fonts[index], run, plan)?))
                    }
                    (Backend::Local, FontSet::Local(fonts))
                    | (Backend::Optimized, FontSet::Optimized(fonts)) => {
                        Ok(Box::new(CoreRun::new(&fonts[index], run, plan)?))
                    }
                    _ => anyhow::bail!("Backend does not match FontSet"),
                }
            })
            .collect::<Result<_>>()?;
        Ok(Self { runs })
    }

    #[inline(never)]
    pub fn shape(&mut self, fresh_buffer: bool) -> Result<usize> {
        let mut count = 0;
        for run in &mut self.runs {
            count += black_box(run.shape(fresh_buffer)?);
        }
        Ok(count)
    }

    pub fn glyphs(&self) -> Vec<Vec<Glyph>> {
        self.runs.iter().map(|run| run.glyphs()).collect()
    }

    pub fn prepare_buffers(&mut self) -> Result<()> {
        for run in &mut self.runs {
            run.prepare_buffer(false)?;
        }
        Ok(())
    }

    pub fn execute(&mut self) -> Result<usize> {
        let mut count = 0;
        for run in &mut self.runs {
            count += black_box(run.execute()?);
        }
        Ok(count)
    }

    pub fn measure_plans(&self) {
        for run in &self.runs {
            run.measure_plan();
        }
    }

    pub fn trace(&mut self) -> Result<()> {
        for run in &mut self.runs {
            run.trace()?;
        }
        Ok(())
    }
}

pub fn harfbuzz_version() -> String {
    harfbuzz::version()
}

pub fn harfbuzz_assertions_enabled() -> bool {
    // SAFETY: 引数と共有状態を持たないC関数
    unsafe { hb_ffi::shaping_lab_harfbuzz_assertions_enabled() != 0 }
}

pub fn shape_once(
    backend: Backend,
    corpus: &LoadedCorpus,
    case: &Case,
    plan: PlanMode,
) -> Result<Vec<Vec<Glyph>>> {
    let fonts = FontSet::new(backend, corpus, case)?;
    let mut session = Session::new(backend, &fonts, case, plan)?;
    session.shape(false)?;
    Ok(session.glyphs())
}

struct CoreRun<'a> {
    input: &'a crate::corpus::Run,
    run: crate::shaper::PreparedRun<'a>,
    shaper: crate::shaper::Shaper,
}

impl<'a> CoreRun<'a> {
    fn new(
        font: &'a crate::shaper::Font,
        input: &'a crate::corpus::Run,
        plan: PlanMode,
    ) -> Result<Self> {
        Ok(Self {
            input,
            run: font.prepare_with_plan(input.properties()?, plan == PlanMode::Cached)?,
            shaper: crate::shaper::Shaper::default(),
        })
    }
}

impl RunEngine for CoreRun<'_> {
    fn shape(&mut self, fresh_buffer: bool) -> Result<usize> {
        self.prepare_buffer(fresh_buffer)?;
        self.execute()
    }

    fn prepare_buffer(&mut self, fresh_buffer: bool) -> Result<()> {
        if fresh_buffer {
            self.shaper = crate::shaper::Shaper::default();
        }
        self.shaper.prepare_buffer(
            &self.run,
            &self.input.text,
            crate::shaper::Context {
                before: &self.input.pre_context,
                after: &self.input.post_context,
            },
        );
        Ok(())
    }

    fn execute(&mut self) -> Result<usize> {
        self.shaper.execute(&self.run)
    }
    fn measure_plan(&self) {
        self.run.measure_plan();
    }
    fn glyphs(&self) -> Vec<Glyph> {
        buffer_glyphs(self.shaper.buffer())
    }

    fn trace(&mut self) -> Result<()> {
        #[cfg(feature = "trace")]
        {
            use std::io::Write;
            self.shaper
                .buffer_mut()
                .set_message_function(Some(Box::new(|_, _, message| {
                    let _ = writeln!(std::io::stderr().lock(), "{message}");
                    true
                })));
            Ok(())
        }
        #[cfg(not(feature = "trace"))]
        anyhow::bail!("Local tracing requires --features trace")
    }
}

fn buffer_glyphs(buffer: &shaping_core::Buffer) -> Vec<Glyph> {
    buffer
        .glyph_infos()
        .iter()
        .zip(buffer.glyph_positions())
        .map(|(i, p)| Glyph {
            id: i.glyph_id,
            cluster: i.cluster,
            x_advance: p.x_advance,
            y_advance: p.y_advance,
            x_offset: p.x_offset,
            y_offset: p.y_offset,
            flags: u32::from(i.unsafe_to_break())
                | (u32::from(i.unsafe_to_concat()) << 1)
                | (u32::from(i.safe_to_insert_tatweel()) << 2),
        })
        .collect()
}
