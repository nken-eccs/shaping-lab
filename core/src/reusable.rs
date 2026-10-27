//! フォントとプランとバッファの再利用

use crate as shaping;
use anyhow::{ensure, Result};
use std::{
    cell::{OnceCell, RefCell},
    collections::VecDeque,
    rc::Rc,
};
use std::{
    ops::Range,
    sync::{mpsc, Arc},
    thread,
};

#[derive(Clone, Copy, Debug)]
pub struct CacheOptions {
    pub plans_per_font: usize,
    pub horizontal_advances: bool,
    pub vertical_origins: bool,
}

impl Default for CacheOptions {
    fn default() -> Self {
        Self {
            plans_per_font: 16,
            horizontal_advances: true,
            vertical_origins: true,
        }
    }
}

/// 字形に関わる設定 テキストと前後文脈はshape時に指定
#[derive(Clone)]
pub struct Properties {
    pub direction: shaping::Direction,
    pub script: Option<shaping::Script>,
    pub language: Option<shaping::Language>,
    pub features: Vec<shaping::Feature>,
    pub cluster_level: shaping::ClusterLevel,
    pub flags: shaping::BufferFlags,
}

impl Default for Properties {
    fn default() -> Self {
        Self {
            direction: shaping::Direction::LeftToRight,
            script: None,
            language: None,
            features: Vec::new(),
            cluster_level: shaping::ClusterLevel::MonotoneGraphemes,
            flags: shaping::BufferFlags::empty(),
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct Context<'a> {
    pub before: &'a str,
    pub after: &'a str,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub plans: usize,
    pub plan_hits: usize,
    pub plan_misses: usize,
    pub horizontal_glyphs: usize,
    pub vertical_glyphs: usize,
}

/// 不変のフォントインスタンス 座標系はUPEM
pub struct Font {
    inner: shaping::Font,
    options: CacheOptions,
    plans: RefCell<VecDeque<Rc<shaping::ShapePlan>>>,
    stats: RefCell<CacheStats>,
    metrics: Metrics,
}

impl Font {
    pub fn new(font: shaping::Font) -> Self {
        Self::with_options(font, CacheOptions::default())
    }

    pub fn with_options(font: shaping::Font, options: CacheOptions) -> Self {
        let metrics = Metrics {
            // 既定位置ではHarfRustのバッチ処理を維持
            horizontal: options.horizontal_advances
                && font
                    .normalized_coords()
                    .iter()
                    .any(|coord| coord.to_bits() != 0),
            vertical: options.vertical_origins,
            advances: RefCell::new(GlyphCache::new(font.num_glyphs())),
            origins: RefCell::new(GlyphCache::new(font.num_glyphs())),
        };
        Self {
            inner: font,
            options,
            plans: RefCell::default(),
            stats: RefCell::default(),
            metrics,
        }
    }

    pub fn prepare(&self, properties: Properties) -> Result<PreparedRun<'_>> {
        self.prepare_with_plan(properties, true)
    }

    pub fn prepare_with_plan(
        &self,
        properties: Properties,
        cached: bool,
    ) -> Result<PreparedRun<'_>> {
        ensure!(
            properties.direction != shaping::Direction::Invalid,
            "Invalid shaping direction"
        );
        Ok(PreparedRun {
            font: self.shaper_font(&properties),
            plan: cached.then(|| self.cached_plan(&properties)),
            properties,
        })
    }

    fn cached_plan(&self, properties: &Properties) -> Rc<shaping::ShapePlan> {
        let key = shaping::ShapePlanKey::new(&self.inner, properties.script, properties.direction)
            .language(properties.language.as_ref())
            .features(&properties.features);
        let mut plans = self.plans.borrow_mut();
        let plan = if let Some(index) = plans.iter().position(|plan| key.matches(plan)) {
            self.stats.borrow_mut().plan_hits += 1;
            plans.remove(index).unwrap()
        } else {
            self.stats.borrow_mut().plan_misses += 1;
            Rc::new(shaping::ShapePlan::new(
                &self.inner,
                properties.direction,
                properties.script,
                properties.language.as_ref(),
                &properties.features,
            ))
        };
        if self.options.plans_per_font > 0 {
            plans.push_front(plan.clone());
            plans.truncate(self.options.plans_per_font);
        }
        plan
    }

    fn shaper_font(&self, properties: &Properties) -> shaping::ShaperFont<'_, '_> {
        let mut font = shaping::ShaperFont::new(&self.inner);
        if self.metrics.horizontal
            || self.metrics.vertical
                && matches!(
                    properties.direction,
                    shaping::Direction::TopToBottom | shaping::Direction::BottomToTop
                )
        {
            font.set_font_funcs(Some(&self.metrics));
        }
        font
    }

    pub fn cache_stats(&self) -> CacheStats {
        CacheStats {
            plans: self.plans.borrow().len(),
            horizontal_glyphs: self.metrics.advances.borrow().entries,
            vertical_glyphs: self.metrics.origins.borrow().entries,
            ..*self.stats.borrow()
        }
    }

    /// 保持中のPreparedRunは引き続き利用可能
    pub fn clear_caches(&self) {
        self.plans.borrow_mut().clear();
        self.metrics.advances.borrow_mut().clear();
        self.metrics.origins.borrow_mut().clear();
        *self.stats.borrow_mut() = CacheStats::default();
    }
}

pub struct PreparedRun<'a> {
    font: shaping::ShaperFont<'a, 'a>,
    properties: Properties,
    plan: Option<Rc<shaping::ShapePlan>>,
}

impl PreparedRun<'_> {
    pub fn measure_plan(&self) {
        drop(std::hint::black_box(shaping::ShapePlan::new(
            &self.font,
            self.properties.direction,
            self.properties.script,
            self.properties.language.as_ref(),
            &self.properties.features,
        )));
    }
}

#[derive(Default)]
pub struct Shaper {
    buffer: shaping::Buffer,
}

impl Shaper {
    /// 出力は次のshapeまで有効 クラスタは入力内のUTF-8バイト位置
    pub fn shape(
        &mut self,
        run: &PreparedRun<'_>,
        text: &str,
        context: Context<'_>,
    ) -> Result<&shaping::Buffer> {
        self.prepare_buffer(run, text, context);
        self.execute(run)?;
        Ok(&self.buffer)
    }

    pub fn buffer(&self) -> &shaping::Buffer {
        &self.buffer
    }

    #[cfg(feature = "trace")]
    pub fn buffer_mut(&mut self) -> &mut shaping::Buffer {
        &mut self.buffer
    }

    pub fn prepare_buffer(&mut self, run: &PreparedRun<'_>, text: &str, context: Context<'_>) {
        self.prepare_text(&run.properties, text, context);
    }

    fn prepare_text(&mut self, properties: &Properties, text: &str, context: Context<'_>) {
        self.buffer.reset();
        self.buffer.push_str(text);
        self.buffer.set_pre_context(context.before);
        self.buffer.set_post_context(context.after);
        self.buffer.set_direction(properties.direction);
        self.buffer.set_script(properties.script);
        self.buffer.set_language(properties.language.clone());
        self.buffer.set_cluster_level(properties.cluster_level);
        self.buffer.set_flags(properties.flags);
    }

    pub fn execute(&mut self, run: &PreparedRun<'_>) -> Result<usize> {
        self.execute_plan(&run.font, &run.properties, run.plan.as_deref())
    }

    fn execute_plan(
        &mut self,
        font: &shaping::ShaperFont<'_, '_>,
        properties: &Properties,
        plan: Option<&shaping::ShapePlan>,
    ) -> Result<usize> {
        shaping::shape(
            font,
            &mut self.buffer,
            shaping::ShapeOptions::new()
                .features(&properties.features)
                .plan(plan),
        )
        .map_err(|error| anyhow::anyhow!("Shaping: {error:?}"))?;
        ensure!(self.buffer.allocation_successful(), "Shaping failed");
        Ok(self.buffer.len())
    }
}

// グリフIDの256個ごとのページ 使用したページのみ確保
struct GlyphCache<T: Copy> {
    pages: Vec<Option<Box<[Option<T>; 256]>>>,
    limit: u32,
    entries: usize,
}

impl<T: Copy> GlyphCache<T> {
    fn new(limit: u32) -> Self {
        Self {
            pages: Vec::new(),
            limit,
            entries: 0,
        }
    }

    fn get_or_insert(&mut self, glyph: shaping::GlyphId, compute: impl FnOnce() -> T) -> T {
        let id = glyph.to_u32();
        if id >= self.limit {
            return compute();
        }
        if self.pages.is_empty() {
            self.pages
                .resize_with(self.limit.div_ceil(256) as usize, || None);
        }
        let page = self.pages[id as usize / 256].get_or_insert_with(|| Box::new([None; 256]));
        *page[id as usize % 256].get_or_insert_with(|| {
            self.entries += 1;
            compute()
        })
    }

    fn clear(&mut self) {
        self.pages.clear();
        self.entries = 0;
    }
}

struct Metrics {
    horizontal: bool,
    vertical: bool,
    advances: RefCell<GlyphCache<i32>>,
    origins: RefCell<GlyphCache<(i32, i32)>>,
}

impl shaping::FontFuncs for Metrics {
    fn nominal_glyphs(
        &self,
        font: &shaping::ShaperFont,
        glyphs: shaping::NominalGlyphs<'_>,
    ) -> usize {
        font.default_nominal_glyphs(glyphs)
    }

    fn glyph_h_advance(&self, font: &shaping::ShaperFont, glyph: shaping::GlyphId) -> i32 {
        if self.horizontal {
            self.advances
                .borrow_mut()
                .get_or_insert(glyph, || font.default_glyph_h_advance(glyph))
        } else {
            font.default_glyph_h_advance(glyph)
        }
    }

    fn glyph_h_advances(&self, font: &shaping::ShaperFont, advances: shaping::Advances<'_>) {
        if !self.horizontal {
            return font.default_glyph_h_advances(advances);
        }
        let mut cache = self.advances.borrow_mut();
        for (glyph, advance) in advances {
            *advance = cache.get_or_insert(glyph, || font.default_glyph_h_advance(glyph));
        }
    }

    fn glyph_v_origin(&self, font: &shaping::ShaperFont, glyph: shaping::GlyphId) -> (i32, i32) {
        // 元の既定経路を保持 コールバック経由の丸め変更を回避
        let compute = || {
            font.clone()
                .with_font_funcs(None)
                .default_glyph_v_origin(glyph)
        };
        if self.vertical {
            self.origins.borrow_mut().get_or_insert(glyph, compute)
        } else {
            compute()
        }
    }
}

#[derive(Clone)]
pub struct RunConfig {
    pub font: usize,
    pub properties: Properties,
}

/// range外の文字列を前後文脈として使用 クラスタはrange先頭からの位置
#[derive(Clone)]
pub struct RunInput {
    pub config: usize,
    pub text: Arc<str>,
    pub range: Range<usize>,
}

#[derive(Clone, Copy)]
pub struct RunOutput<'a> {
    pub infos: &'a [shaping::GlyphInfo],
    pub positions: &'a [shaping::GlyphPosition],
}

#[derive(Default)]
struct BatchChunk {
    start: usize,
    ranges: Vec<Range<usize>>,
    infos: Vec<shaping::GlyphInfo>,
    positions: Vec<shaping::GlyphPosition>,
}

impl BatchChunk {
    fn clear(&mut self) {
        self.ranges.clear();
        self.infos.clear();
        self.positions.clear();
    }

    fn get(&self, index: usize) -> Option<RunOutput<'_>> {
        let range = self.ranges.get(index)?;
        Some(RunOutput {
            infos: &self.infos[range.clone()],
            positions: &self.positions[range.clone()],
        })
    }
}

#[derive(Default)]
pub struct BatchOutput {
    chunks: Vec<BatchChunk>,
    len: usize,
}

impl BatchOutput {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn get(&self, index: usize) -> Option<RunOutput<'_>> {
        if index >= self.len {
            return None;
        }
        self.chunks
            .iter()
            .find_map(|chunk| index.checked_sub(chunk.start).and_then(|i| chunk.get(i)))
    }

    pub fn iter(&self) -> impl Iterator<Item = RunOutput<'_>> {
        self.chunks
            .iter()
            .flat_map(|chunk| (0..chunk.ranges.len()).map(|index| chunk.get(index).unwrap()))
    }
}

struct BatchRun<'a> {
    font: shaping::ShaperFont<'a, 'a>,
    properties: &'a Properties,
    plan: &'a shaping::ShapePlan,
}

struct BatchState {
    fonts: Vec<Font>,
    configs: Arc<[RunConfig]>,
    plans: Vec<OnceCell<Rc<shaping::ShapePlan>>>,
    shaper: Shaper,
}

impl BatchState {
    fn new(fonts: Vec<shaping::Font>, configs: Arc<[RunConfig]>) -> Self {
        let mut counts = vec![0; fonts.len()];
        for config in &*configs {
            counts[config.font] += 1;
        }
        let fonts = fonts
            .into_iter()
            .zip(counts)
            .map(|(font, count)| {
                Font::with_options(
                    font,
                    CacheOptions {
                        plans_per_font: count.max(16),
                        ..Default::default()
                    },
                )
            })
            .collect::<Vec<_>>();
        let plans = (0..configs.len()).map(|_| OnceCell::new()).collect();
        Self {
            fonts,
            configs,
            plans,
            shaper: Shaper::default(),
        }
    }

    fn shape(&mut self, inputs: &[RunInput], start: usize, output: &mut BatchChunk) -> Result<()> {
        output.clear();
        output.start = start;
        let mut prepared = (0..self.configs.len())
            .map(|_| None)
            .collect::<smallvec::SmallVec<[Option<BatchRun<'_>>; 4]>>();
        for input in inputs {
            let run = prepared
                .get_mut(input.config)
                .ok_or_else(|| anyhow::anyhow!("Unknown run configuration: {}", input.config))?
                .get_or_insert_with(|| {
                    let config = &self.configs[input.config];
                    let font = &self.fonts[config.font];
                    let plan = self.plans[input.config]
                        .get_or_init(|| font.cached_plan(&config.properties));
                    BatchRun {
                        font: font.shaper_font(&config.properties),
                        properties: &config.properties,
                        plan,
                    }
                });
            let text = input
                .text
                .get(input.range.clone())
                .ok_or_else(|| anyhow::anyhow!("Invalid UTF-8 run range"))?;
            self.shaper.prepare_text(
                run.properties,
                text,
                Context {
                    before: &input.text[..input.range.start],
                    after: &input.text[input.range.end..],
                },
            );
            self.shaper
                .execute_plan(&run.font, run.properties, Some(run.plan))?;
            let buffer = self.shaper.buffer();
            let offset = output.infos.len();
            output.infos.extend_from_slice(buffer.glyph_infos());
            output.positions.extend_from_slice(buffer.glyph_positions());
            output.ranges.push(offset..output.infos.len());
        }
        Ok(())
    }
}

struct BatchJob {
    inputs: Arc<[RunInput]>,
    range: Range<usize>,
    output: BatchChunk,
}

struct BatchWorker {
    sender: Option<mpsc::Sender<BatchJob>>,
    receiver: mpsc::Receiver<Result<BatchChunk>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Drop for BatchWorker {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// 常駐ワーカーで短いRunをまとめて処理 出力の順序は入力順
pub struct BatchShaper {
    local: BatchState,
    workers: Vec<BatchWorker>,
    output: BatchOutput,
    min_runs_per_worker: usize,
}

impl BatchShaper {
    /// workersには呼び出し元を含む フォントと設定は生成後に固定
    pub fn new(fonts: Vec<shaping::Font>, configs: Vec<RunConfig>, workers: usize) -> Result<Self> {
        ensure!(workers > 0, "Worker count must be positive");
        for config in &configs {
            ensure!(
                config.font < fonts.len(),
                "Unknown font index: {}",
                config.font
            );
            ensure!(
                config.properties.direction != shaping::Direction::Invalid,
                "Invalid shaping direction"
            );
        }
        let configs: Arc<[RunConfig]> = configs.into();
        let local = BatchState::new(fonts.clone(), configs.clone());
        let mut background = Vec::new();
        for index in 1..workers {
            let fonts = fonts.clone();
            let configs = configs.clone();
            let (sender, jobs) = mpsc::channel::<BatchJob>();
            let (replies, receiver) = mpsc::channel();
            let thread = thread::Builder::new()
                .name(format!("shaping-{index}"))
                .spawn(move || {
                    let mut state = BatchState::new(fonts, configs);
                    while let Ok(mut job) = jobs.recv() {
                        let result = state
                            .shape(
                                &job.inputs[job.range.clone()],
                                job.range.start,
                                &mut job.output,
                            )
                            .map(|()| job.output);
                        if replies.send(result).is_err() {
                            break;
                        }
                    }
                })?;
            background.push(BatchWorker {
                sender: Some(sender),
                receiver,
                thread: Some(thread),
            });
        }
        Ok(Self {
            local,
            workers: background,
            output: BatchOutput {
                chunks: (0..workers).map(|_| BatchChunk::default()).collect(),
                len: 0,
            },
            min_runs_per_worker: 32,
        })
    }

    pub fn set_min_runs_per_worker(&mut self, count: usize) -> Result<()> {
        ensure!(count > 0, "Minimum runs per worker must be positive");
        self.min_runs_per_worker = count;
        Ok(())
    }

    /// 入力の所有権を共有 出力配列は次の呼び出しで再利用
    pub fn shape(&mut self, inputs: Arc<[RunInput]>) -> Result<&BatchOutput> {
        self.output.len = 0;
        for chunk in &mut self.output.chunks {
            chunk.clear();
        }
        if inputs.is_empty() {
            return Ok(&self.output);
        }
        let active = (inputs.len() / self.min_runs_per_worker)
            .max(1)
            .min(self.workers.len() + 1);
        let size = inputs.len().div_ceil(active);
        let active = inputs.len().div_ceil(size);
        let mut sent = 0;
        let mut error = None;
        for (index, worker) in self.workers.iter().take(active - 1).enumerate() {
            let start = (index + 1) * size;
            let job = BatchJob {
                inputs: inputs.clone(),
                range: start..(start + size).min(inputs.len()),
                output: std::mem::take(&mut self.output.chunks[index + 1]),
            };
            if worker.sender.as_ref().unwrap().send(job).is_err() {
                error = Some(anyhow::anyhow!("Shaping worker stopped"));
                break;
            }
            sent += 1;
        }
        if let Err(cause) = self
            .local
            .shape(&inputs[..size], 0, &mut self.output.chunks[0])
        {
            error = Some(cause);
        }
        for (index, worker) in self.workers.iter().take(sent).enumerate() {
            match worker.receiver.recv() {
                Ok(Ok(output)) => self.output.chunks[index + 1] = output,
                Ok(Err(cause)) => error = Some(cause),
                Err(_) => error = Some(anyhow::anyhow!("Shaping worker stopped")),
            }
        }
        if let Some(error) = error {
            return Err(error);
        }
        self.output.len = inputs.len();
        Ok(&self.output)
    }
}
