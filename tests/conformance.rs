use shaping_lab::{Backend, FontSet, LoadedCorpus, PlanMode, Session, engine::shape_once, report};

fn corpus() -> LoadedCorpus {
    LoadedCorpus::load(concat!(env!("CARGO_MANIFEST_DIR"), "/corpus/default.toml")).unwrap()
}

#[test]
fn assertion_settings_match_the_profile() {
    assert_eq!(
        shaping_lab::engine::harfbuzz_assertions_enabled(),
        cfg!(debug_assertions)
    );
}

#[test]
fn engines_agree_across_scripts_and_plan_policies() {
    let corpus = corpus();
    for case in &corpus.spec.cases {
        let expected = shape_once(Backend::Harfbuzz, &corpus, case, PlanMode::Cached).unwrap();
        for backend in [Backend::Harfbuzz, Backend::Local, Backend::Optimized] {
            for plan in [PlanMode::Cached, PlanMode::Rebuild, PlanMode::Native] {
                let actual = shape_once(backend, &corpus, case, plan).unwrap();
                assert_eq!(expected, actual, "{} / {backend} / {plan}", case.id);
            }
        }
        if case.id != "missing-glyph" {
            assert!(
                expected.iter().flatten().all(|g| g.id != 0),
                "Unexpected .notdef in {}",
                case.id
            );
        }
    }
}

#[test]
fn reused_and_fresh_buffers_produce_identical_glyphs() {
    let corpus = corpus();
    for backend in [Backend::Harfbuzz, Backend::Local, Backend::Optimized] {
        for case in &corpus.spec.cases {
            let fonts = FontSet::new(backend, &corpus, case).unwrap();
            let mut session = Session::new(backend, &fonts, case, PlanMode::Cached).unwrap();
            session.shape(false).unwrap();
            let expected = session.glyphs();
            for fresh in [false, false, true, false] {
                session.shape(fresh).unwrap();
                assert_eq!(expected, session.glyphs(), "{} / {backend}", case.id);
            }
        }
    }
}

#[test]
fn pair_positioning_preserves_subtable_order_after_warmup() {
    use shaping_lab::corpus::Feature;
    use shaping_lab::shaper::{Context, Font, Shaper};

    let corpus = corpus();
    let texts = [
        "AJ Av AV To Wa",
        "A\u{338}V T\u{338}o V\u{338}a",
        "Vx AV AF Tx Fy ξζ χх",
        "ABCDEFGHIJKLMNOPQRSTUVWX YZ",
        "AVATAR AVATAR AJ AJ Av Av",
    ];
    let mut shaper = Shaper::default();
    for font_name in ["latin", "japanese"] {
        for weight in [400.0, 725.0] {
            for ranged in [false, true] {
                let mut case = corpus.cases(Some("latin-ligatures")).unwrap()[0].clone();
                let run = &mut case.runs[0];
                run.font = font_name.into();
                run.variations.insert("wght".into(), weight);
                if ranged {
                    run.features.push(Feature {
                        tag: "kern".into(),
                        value: 0,
                        start: 0,
                        end: 1,
                    });
                }
                let font = Font::new(
                    corpus.fonts[font_name]
                        .instantiate(&run.variations)
                        .unwrap(),
                );
                let prepared = font.prepare(run.properties().unwrap()).unwrap();
                let expected: Vec<_> = texts
                    .iter()
                    .map(|text| {
                        case.runs[0].text = (*text).into();
                        shape_once(Backend::Harfbuzz, &corpus, &case, PlanMode::Cached).unwrap()
                    })
                    .collect();
                for iteration in 0..texts.len() * 3 {
                    let index = iteration * 3 % texts.len();
                    let actual = shaper
                        .shape(&prepared, texts[index], Context::default())
                        .unwrap();
                    assert_eq!(
                        expected[index][0],
                        core_glyphs(actual),
                        "{font_name} / {weight} / {ranged} / {index}"
                    );
                }
            }
        }
    }
}

#[test]
fn changing_multilingual_runs_preserve_output_after_warmup() {
    use shaping_lab::shaper::{Context, Font, Shaper};
    let corpus = LoadedCorpus::load(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/corpus/multilingual.toml"
    ))
    .unwrap();
    let mut shaper = Shaper::default();
    for source in &corpus.spec.cases[0].runs {
        let font = Font::new(
            corpus.fonts[&source.font]
                .instantiate(&source.variations)
                .unwrap(),
        );
        for unsafe_concat in [false, true] {
            let mut properties = source.properties().unwrap();
            if unsafe_concat {
                properties.flags |= shaping_lab::shaping::BufferFlags::PRODUCE_UNSAFE_TO_CONCAT;
            }
            let prepared = font.prepare(properties).unwrap();
            let boundaries = source
                .text
                .char_indices()
                .map(|(i, _)| i)
                .chain([source.text.len()])
                .collect::<Vec<_>>();
            for count in [4, 16, 31, 48, 63, 64, 65, 129, boundaries.len() - 1, 16, 4] {
                let start = if count < 129 { boundaries.len() / 4 } else { 0 };
                let range =
                    boundaries[start]..boundaries[(start + count).min(boundaries.len() - 1)];
                let mut case = corpus.spec.cases[0].clone();
                let mut input = source.clone();
                input.text = source.text[range.clone()].to_owned();
                input.pre_context = source.text[..range.start].to_owned();
                input.post_context = source.text[range.end..].to_owned();
                if unsafe_concat {
                    input
                        .flags
                        .push(shaping_lab::corpus::BufferFlag::UnsafeToConcat);
                }
                case.runs = vec![input];
                let expected =
                    shape_once(Backend::Optimized, &corpus, &case, PlanMode::Cached).unwrap();
                if !unsafe_concat {
                    let native =
                        shape_once(Backend::Harfbuzz, &corpus, &case, PlanMode::Cached).unwrap();
                    assert_eq!(native, expected, "Cold mismatch: {} / {count}", source.font);
                }
                for _ in 0..3 {
                    let actual = shaper
                        .shape(
                            &prepared,
                            &source.text[range.clone()],
                            Context {
                                before: &source.text[..range.start],
                                after: &source.text[range.end..],
                            },
                        )
                        .unwrap();
                    assert_eq!(
                        expected[0],
                        core_glyphs(actual),
                        "{} / {} / {count}",
                        source.font,
                        source.language
                    );
                }
            }
        }
    }
}

#[test]
fn split_stages_preserve_output() {
    let corpus = corpus();
    for case in &corpus.spec.cases {
        let expected = shape_once(Backend::Harfbuzz, &corpus, case, PlanMode::Cached).unwrap();
        for backend in [Backend::Harfbuzz, Backend::Local, Backend::Optimized] {
            let fonts = FontSet::new(backend, &corpus, case).unwrap();
            let mut session = Session::new(backend, &fonts, case, PlanMode::Cached).unwrap();
            for _ in 0..3 {
                session.prepare_buffers().unwrap();
                session.measure_plans();
                session.execute().unwrap();
                assert_eq!(expected, session.glyphs(), "{} / {backend}", case.id);
            }
        }
    }
}

#[test]
fn shared_plan_dispatch_is_consistent_across_threads() {
    use shaping_lab::shaping;
    fn shape(
        font: &shaping::Font,
        plan: &shaping::ShapePlan,
        text: &str,
        buffer: &mut shaping::Buffer,
    ) -> Vec<shaping_lab::engine::Glyph> {
        buffer.clear();
        buffer.push_str(text);
        buffer.set_direction(shaping::Direction::LeftToRight);
        buffer.set_script(Some("Latn".parse().unwrap()));
        buffer.set_language(Some("en".parse().unwrap()));
        shaping::shape(
            &shaping::ShaperFont::new(font),
            buffer,
            shaping::ShapeOptions::new().plan(Some(plan)),
        )
        .unwrap();
        core_glyphs(buffer)
    }
    let corpus = corpus();
    let font = corpus.fonts["latin"]
        .instantiate(&Default::default())
        .unwrap();
    let plan = shaping::ShapePlan::new(
        &font,
        shaping::Direction::LeftToRight,
        Some("Latn".parse().unwrap()),
        Some(&"en".parse().unwrap()),
        &[],
    );
    let texts = [
        "Rust typography office affine AVATAR",
        "ÀÁÂÃÄÅ ÆÇ ÈÉÊË ÌÍÎÏ ÐÑ ÒÓÔÕÖ ØÙÚÛÜ ÝÞß",
        "àáâãäå æç èéêë ìíîï ðñ òóôõö øùúûü ýþÿ",
        "Āā Ăă Ąą Ćć Ĉĉ Ċċ Čč Ďď Đđ Ēē Ĕĕ Ėė Ęę Ěě",
        "Tiếng Việt được viết bằng nhiều dấu thanh",
        "a\u{301}e\u{308}i\u{302}o\u{303}u\u{30a} office",
    ];
    let expected: Vec<_> = texts
        .iter()
        .map(|text| shape(&font, &plan, text, &mut shaping::Buffer::new()))
        .collect();
    std::thread::scope(|scope| {
        for worker in 0..4 {
            let (font, plan, expected, texts) = (&font, &plan, &expected, &texts);
            scope.spawn(move || {
                let mut buffer = shaping::Buffer::new();
                for i in 0..256 {
                    let index = (i * 7 + worker * 3) % texts.len();
                    assert_eq!(
                        shape(font, plan, texts[index], &mut buffer),
                        expected[index]
                    );
                }
            });
        }
    });
}

#[test]
fn retained_plan_does_not_keep_font_bytes_alive() {
    use shaping_lab::shaping;
    use std::sync::Arc;
    let corpus = corpus();
    let bytes = Arc::new(corpus.fonts["latin"].bytes.as_ref().clone());
    let weak = Arc::downgrade(&bytes);
    let font = shaping::Font::new(shaping::font::Blob::Shared(bytes), 0).unwrap();
    let plan = shaping::ShapePlan::new(
        &font,
        shaping::Direction::LeftToRight,
        Some("Latn".parse().unwrap()),
        None,
        &[],
    );
    let mut buffer = shaping::Buffer::new();
    for _ in 0..3 {
        buffer.clear();
        buffer.push_str("Rust typography office affine AVATAR");
        buffer.set_direction(shaping::Direction::LeftToRight);
        buffer.set_script(Some("Latn".parse().unwrap()));
        shaping::shape(
            &shaping::ShaperFont::new(&font),
            &mut buffer,
            shaping::ShapeOptions::new().plan(Some(&plan)),
        )
        .unwrap();
    }
    drop(font);
    assert!(weak.upgrade().is_none());
    drop(plan);
}

#[test]
fn indexed_runs_preserve_feature_ranges_variations_and_vertical_output() {
    let corpus = corpus();
    for name in [
        "latin-range-feature",
        "latin-variable",
        "japanese-vertical",
        "arabic-context",
    ] {
        for copies in [2, 4, 16] {
            let mut case = corpus.cases(Some(name)).unwrap()[0].clone();
            for run in &mut case.runs {
                run.text = run.text.repeat(copies);
            }
            let expected = shape_once(Backend::Harfbuzz, &corpus, &case, PlanMode::Cached).unwrap();
            let fonts = FontSet::new(Backend::Optimized, &corpus, &case).unwrap();
            let mut session =
                Session::new(Backend::Optimized, &fonts, &case, PlanMode::Cached).unwrap();
            for _ in 0..3 {
                session.shape(false).unwrap();
                assert_eq!(session.glyphs(), expected, "{name} / {copies}");
            }
        }
    }
}

#[test]
fn vertical_cache_respects_font_instances() {
    let corpus = corpus();
    for weight in [100.0, 400.0, 900.0] {
        let mut case = corpus.cases(Some("japanese-vertical")).unwrap()[0].clone();
        case.runs[0].variations.insert("wght".into(), weight);
        let expected = shape_once(Backend::Local, &corpus, &case, PlanMode::Cached).unwrap();
        let fonts = FontSet::new(Backend::Optimized, &corpus, &case).unwrap();
        let mut session =
            Session::new(Backend::Optimized, &fonts, &case, PlanMode::Cached).unwrap();
        for _ in 0..3 {
            session.shape(false).unwrap();
            assert_eq!(expected, session.glyphs(), "weight {weight}");
        }
    }
}

#[test]
fn features_variations_context_and_direction_are_effective() {
    let corpus = corpus();
    let get = |name| corpus.cases(Some(name)).unwrap()[0];
    for backend in [Backend::Harfbuzz, Backend::Local] {
        let shape = |case| shape_once(backend, &corpus, case, PlanMode::Cached).unwrap();
        let ligatures = shape(get("latin-ligatures"));
        let no_ligatures = shape(get("latin-no-ligatures"));
        assert!(ligatures[0].len() < no_ligatures[0].len());
        let variable = get("latin-variable");
        let mut regular = variable.clone();
        regular.runs[0].variations.clear();
        assert_ne!(shape(variable), shape(&regular));
        let context = get("arabic-context");
        let mut isolated = context.clone();
        for run in &mut isolated.runs {
            run.pre_context.clear();
            run.post_context.clear();
        }
        assert_ne!(shape(context), shape(&isolated));
        for (run, glyphs) in context.runs.iter().zip(shape(context)) {
            assert!(
                glyphs
                    .iter()
                    .all(|g| run.text.is_char_boundary(g.cluster as usize))
            );
            assert!(glyphs.windows(2).all(|g| g[0].cluster >= g[1].cluster));
        }
        let vertical = shape(get("japanese-vertical"));
        assert!(
            vertical[0]
                .iter()
                .all(|g| g.x_advance == 0 && g.y_advance < 0)
        );
        assert_eq!(
            shape(get("missing-glyph"))[0]
                .iter()
                .filter(|g| g.id == 0)
                .count(),
            1
        );
    }
}

#[test]
fn invalid_corpus_is_rejected() {
    let original = corpus().spec;
    let mut spec = original.clone();
    spec.cases.push(spec.cases[0].clone());
    assert!(spec.validate().is_err());
    let mut spec = original.clone();
    spec.cases[0].runs[0].font = "absent".into();
    assert!(spec.validate().is_err());
    let mut spec = original.clone();
    let run = &mut spec
        .cases
        .iter_mut()
        .find(|c| c.id == "latin-range-feature")
        .unwrap()
        .runs[0];
    run.features[0].start = 1;
    assert!(spec.validate().is_err());
    let mut spec = original;
    spec.cases[0].runs[0]
        .variations
        .insert("wght".into(), f32::NAN);
    assert!(spec.validate().is_err());
}

#[test]
fn differences_include_positions_and_missing_glyphs() {
    let corpus = corpus();
    let case = corpus.cases(Some("latin-ligatures")).unwrap()[0];
    let left = shape_once(Backend::Harfbuzz, &corpus, case, PlanMode::Cached).unwrap();
    let mut right = left.clone();
    right[0][0].x_advance += 1;
    right[0].pop();
    let differences = report::diff(&left, &right);
    assert_eq!(differences.len(), 2);
    assert_eq!(differences[0].glyph, 0);
    assert!(differences[1].right.is_none());
}

#[test]
fn script_case_and_context_feature_ranges_are_consistent() {
    let corpus = corpus();
    let mut case = corpus.cases(Some("latin-range-feature")).unwrap()[0].clone();
    let expected = shape_once(Backend::Harfbuzz, &corpus, &case, PlanMode::Cached).unwrap();
    case.runs[0].script = "latn".into();
    case.runs[0].pre_context = "before ".into();
    case.runs[0].post_context = " after".into();
    for backend in [Backend::Harfbuzz, Backend::Local] {
        assert_eq!(
            expected,
            shape_once(backend, &corpus, &case, PlanMode::Cached).unwrap()
        );
    }
}

fn core_glyphs(buffer: &shaping_core::Buffer) -> Vec<shaping_lab::engine::Glyph> {
    run_glyphs(shaping_lab::shaper::RunOutput {
        infos: buffer.glyph_infos(),
        positions: buffer.glyph_positions(),
    })
}

fn run_glyphs(output: shaping_lab::shaper::RunOutput<'_>) -> Vec<shaping_lab::engine::Glyph> {
    output
        .infos
        .iter()
        .zip(output.positions)
        .map(|(i, p)| shaping_lab::engine::Glyph {
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

#[test]
fn parallel_batches_preserve_order_context_and_glyphs() {
    use shaping_lab::shaper::{BatchShaper, Context, Font, RunConfig, RunInput, Shaper};
    use std::sync::Arc;
    let corpus = corpus();
    let mut fonts = Vec::new();
    let mut configs = Vec::new();
    let mut templates = Vec::new();
    for name in [
        "mixed-fonts",
        "latin-variable",
        "japanese-vertical",
        "arabic-context",
        "latin-range-feature",
    ] {
        for run in &corpus.cases(Some(name)).unwrap()[0].runs {
            let index = configs.len();
            fonts.push(
                corpus.fonts[&run.font]
                    .instantiate(&run.variations)
                    .unwrap(),
            );
            configs.push(RunConfig {
                font: index,
                properties: run.properties().unwrap(),
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
    }
    let serial_fonts = fonts.iter().cloned().map(Font::new).collect::<Vec<_>>();
    let prepared = configs
        .iter()
        .map(|config| {
            serial_fonts[config.font]
                .prepare(config.properties.clone())
                .unwrap()
        })
        .collect::<Vec<_>>();
    let mut serial = Shaper::default();
    let expected = templates
        .iter()
        .map(|input| {
            core_glyphs(
                serial
                    .shape(
                        &prepared[input.config],
                        &input.text[input.range.clone()],
                        Context {
                            before: &input.text[..input.range.start],
                            after: &input.text[input.range.end..],
                        },
                    )
                    .unwrap(),
            )
        })
        .collect::<Vec<_>>();
    for workers in [1, 2, 4] {
        let mut batch = BatchShaper::new(fonts.clone(), configs.clone(), workers).unwrap();
        for minimum in [1, 256] {
            batch.set_min_runs_per_worker(minimum).unwrap();
            for count in [0, 1, 5, 31, 513, 1031] {
                let inputs: Arc<[RunInput]> = (0..count)
                    .map(|i| templates[(i * 7 + count) % templates.len()].clone())
                    .collect();
                let output = batch.shape(inputs).unwrap();
                assert_eq!(output.len(), count);
                assert_eq!(output.is_empty(), count == 0);
                assert_eq!(output.iter().count(), count);
                assert!(output.get(count).is_none());
                for (i, actual) in output.iter().enumerate() {
                    assert_eq!(
                        expected[(i * 7 + count) % templates.len()],
                        run_glyphs(actual),
                        "workers {workers} / minimum {minimum} / count {count} / index {i}"
                    );
                    assert_eq!(run_glyphs(actual), run_glyphs(output.get(i).unwrap()));
                }
            }
        }
    }
}

#[test]
fn batch_errors_do_not_leave_pending_work() {
    use shaping_lab::shaper::{BatchShaper, Properties, RunConfig, RunInput};
    use std::sync::Arc;
    assert!(BatchShaper::new(vec![], vec![], 0).is_err());
    assert!(
        BatchShaper::new(
            vec![],
            vec![RunConfig {
                font: 0,
                properties: Properties::default()
            }],
            1
        )
        .is_err()
    );
    let corpus = corpus();
    let raw = corpus.fonts["japanese"]
        .instantiate(&Default::default())
        .unwrap();
    let mut batch = BatchShaper::new(
        vec![raw],
        vec![RunConfig {
            font: 0,
            properties: Properties::default(),
        }],
        4,
    )
    .unwrap();
    assert!(batch.set_min_runs_per_worker(0).is_err());
    batch.set_min_runs_per_worker(1).unwrap();
    let valid = RunInput {
        config: 0,
        text: Arc::from("日本語"),
        range: 0..9,
    };
    for broken in [
        RunInput {
            config: 9,
            ..valid.clone()
        },
        RunInput {
            range: 1..3,
            ..valid.clone()
        },
    ] {
        let mut inputs = vec![valid.clone(); 17];
        inputs[10] = broken;
        assert!(batch.shape(inputs.into()).is_err());
        assert_eq!(batch.shape(vec![valid.clone(); 5].into()).unwrap().len(), 5);
    }
}

#[test]
fn one_buffer_handles_changing_text_fonts_and_context() {
    use shaping_lab::shaper::{Context, Font, Shaper};
    let corpus = corpus();
    let key = |run: &shaping_lab::corpus::Run| format!("{}:{:?}", run.font, run.variations);
    let mut fonts = std::collections::BTreeMap::new();
    for run in corpus.spec.cases.iter().flat_map(|case| &case.runs) {
        fonts.entry(key(run)).or_insert_with(|| {
            Font::new(
                corpus.fonts[&run.font]
                    .instantiate(&run.variations)
                    .unwrap(),
            )
        });
    }
    let mut shaper = Shaper::default();
    for phase in [0, 1, 2, 0] {
        for case in &corpus.spec.cases {
            for original in &case.runs {
                let mut run = original.clone();
                let split = run
                    .text
                    .char_indices()
                    .nth(run.text.chars().count() / 2)
                    .map_or(0, |(i, _)| i);
                if phase == 1 {
                    run.post_context = format!("{}{}", &run.text[split..], run.post_context);
                    run.text.truncate(split);
                } else if phase == 2 {
                    run.pre_context.push_str(&run.text[..split]);
                    run.text = run.text[split..].into();
                }
                let mut single = case.clone();
                single.runs = vec![run.clone()];
                let expected =
                    shape_once(Backend::Local, &corpus, &single, PlanMode::Cached).unwrap();
                let prepared = fonts[&key(&run)]
                    .prepare(run.properties().unwrap())
                    .unwrap();
                let actual = shaper
                    .shape(
                        &prepared,
                        &run.text,
                        Context {
                            before: &run.pre_context,
                            after: &run.post_context,
                        },
                    )
                    .unwrap();
                assert_eq!(
                    expected[0],
                    core_glyphs(actual),
                    "{} / phase {phase}",
                    case.id
                );
            }
        }
    }
    assert!(fonts.values().any(|font| font.cache_stats().plan_hits > 0));
    assert!(
        fonts
            .values()
            .any(|font| font.cache_stats().horizontal_glyphs > 0)
    );
    assert!(
        fonts
            .values()
            .any(|font| font.cache_stats().vertical_glyphs > 0)
    );
}

#[test]
fn plan_keys_eviction_and_cache_reset_preserve_output() {
    use shaping_lab::shaper::{CacheOptions, Context, Font, Shaper};
    let corpus = corpus();
    let case = corpus.cases(Some("latin-ligatures")).unwrap()[0];
    let font = Font::with_options(
        corpus.fonts[&case.runs[0].font]
            .instantiate(&Default::default())
            .unwrap(),
        CacheOptions {
            plans_per_font: 2,
            ..Default::default()
        },
    );
    let properties = case.runs[0].properties().unwrap();
    let retained = font.prepare(properties.clone()).unwrap();
    let mut shaper = Shaper::default();
    let original = core_glyphs(
        shaper
            .shape(&retained, &case.runs[0].text, Context::default())
            .unwrap(),
    );
    for (direction, script, language, value, start, end) in [
        ("ltr", "Latn", "en", 0, 0, u32::MAX),
        ("ltr", "Latn", "tr", 0, 0, u32::MAX),
        ("ltr", "Latn", "tr", 1, 0, u32::MAX),
        ("rtl", "Latn", "tr", 1, 0, u32::MAX),
        ("ltr", "Cyrl", "ru", 1, 0, u32::MAX),
        ("ltr", "Latn", "en", 0, 0, 3),
        ("ltr", "Latn", "en", 0, 4, 10),
    ] {
        let mut changed = case.clone();
        let input = &mut changed.runs[0];
        input.direction = if direction == "rtl" {
            shaping_lab::corpus::Direction::Rtl
        } else {
            shaping_lab::corpus::Direction::Ltr
        };
        input.script = script.into();
        input.language = language.into();
        input.features = vec![shaping_lab::corpus::Feature {
            tag: "liga".into(),
            value,
            start,
            end,
        }];
        let prepared = font.prepare(input.properties().unwrap()).unwrap();
        let actual = core_glyphs(
            shaper
                .shape(&prepared, &input.text, Context::default())
                .unwrap(),
        );
        let expected = shape_once(Backend::Local, &corpus, &changed, PlanMode::Cached).unwrap();
        assert_eq!(expected[0], actual);
        assert!(font.cache_stats().plans <= 2);
    }
    assert_eq!(font.cache_stats().plan_misses, 7);
    assert_eq!(font.cache_stats().plan_hits, 1);
    font.clear_caches();
    assert_eq!(font.cache_stats(), Default::default());
    assert_eq!(
        original,
        core_glyphs(
            shaper
                .shape(&retained, &case.runs[0].text, Context::default())
                .unwrap()
        )
    );
    let invalid = shaping_lab::shaper::Properties {
        direction: shaping_core::Direction::Invalid,
        ..properties
    };
    assert!(font.prepare(invalid).is_err());
}

#[test]
fn caches_are_optional_and_font_switches_share_instances() {
    use shaping_lab::shaper::{CacheOptions, Context, Font, Shaper};
    let corpus = corpus();
    for id in ["latin-variable", "japanese-vertical"] {
        let case = corpus.cases(Some(id)).unwrap()[0];
        for weight in [100.0, 400.0, 900.0] {
            let mut input = case.runs[0].clone();
            input.variations.insert("wght".into(), weight);
            let mut changed = case.clone();
            changed.runs = vec![input.clone()];
            let expected = shape_once(Backend::Local, &corpus, &changed, PlanMode::Cached).unwrap();
            for (horizontal_advances, vertical_origins) in
                [(false, false), (true, false), (false, true), (true, true)]
            {
                let font = Font::with_options(
                    corpus.fonts[&input.font]
                        .instantiate(&input.variations)
                        .unwrap(),
                    CacheOptions {
                        plans_per_font: 0,
                        horizontal_advances,
                        vertical_origins,
                    },
                );
                let mut shaper = Shaper::default();
                for _ in 0..3 {
                    let run = font.prepare(input.properties().unwrap()).unwrap();
                    assert_eq!(
                        expected[0],
                        core_glyphs(shaper.shape(&run, &input.text, Context::default()).unwrap())
                    );
                }
                assert_eq!(font.cache_stats().plans, 0);
                assert_eq!(font.cache_stats().plan_misses, 3);
                if !horizontal_advances {
                    assert_eq!(font.cache_stats().horizontal_glyphs, 0);
                }
                if !vertical_origins {
                    assert_eq!(font.cache_stats().vertical_glyphs, 0);
                }
                font.clear_caches();
                assert_eq!(font.cache_stats(), Default::default());
            }
        }
    }
    let case = corpus.cases(Some("font-switches")).unwrap()[0];
    let fonts = FontSet::new(Backend::Optimized, &corpus, case).unwrap();
    let FontSet::Optimized(fonts) = fonts else {
        unreachable!()
    };
    let mut matches = 0;
    for i in 0..case.runs.len() {
        for j in 0..i {
            if case.runs[i].font == case.runs[j].font
                && case.runs[i].variations == case.runs[j].variations
            {
                assert!(std::rc::Rc::ptr_eq(&fonts[i], &fonts[j]));
                matches += 1;
            }
        }
    }
    assert!(matches > 0);
}

use std::process::Command;

#[test]
fn cli_reports_errors_and_exports_json() {
    let binary = env!("CARGO_BIN_EXE_shape-lab");
    let output = Command::new(binary)
        .args(["--case", "absent", "shape"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Case not found"));
    let output = Command::new(binary)
        .args(["profile", "--backend", "local"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires --case"));
    let output = Command::new(binary)
        .args(["--case", "arabic-context", "shape"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["cases"][0]["runs"].as_array().unwrap().len(), 2);
    assert_eq!(json["metadata"]["backends"]["harfbuzz"], "15.0.0");
}

#[test]
fn trace_emits_events_and_preserves_json() {
    let binary = env!("CARGO_BIN_EXE_shape-lab");
    for backend in ["harfbuzz", "local", "optimized"] {
        let output = Command::new(binary)
            .args(["--case", "arabic", "trace", "--backend", backend])
            .output()
            .unwrap();
        if backend != "harfbuzz" && !cfg!(feature = "trace") {
            assert_eq!(output.status.code(), Some(1));
            assert!(String::from_utf8_lossy(&output.stderr).contains("requires --features trace"));
        } else {
            assert!(output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("start"));
            let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(!json["runs"][0].as_array().unwrap().is_empty());
        }
    }
}
