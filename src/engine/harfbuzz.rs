use super::{Glyph, PlanMode, RunEngine, hb_ffi as hb};
use crate::corpus::{self, LoadedFont};
use anyhow::{Result, ensure};
use std::{
    ffi::{CStr, c_char, c_void},
    io::Write,
    ptr,
    sync::Arc,
};

pub struct Font {
    raw: *mut hb::Handle,
    _bytes: Arc<Vec<u8>>,
}

fn tag(text: &str) -> u32 {
    u32::from_be_bytes(text.as_bytes().try_into().unwrap())
}

impl Font {
    pub fn new(data: &LoadedFont, run: &corpus::Run) -> Result<Self> {
        // SAFETY: 読み取り専用のbytesをFontの破棄まで保持
        unsafe {
            let blob = hb::hb_blob_create(
                data.bytes.as_ptr().cast(),
                data.bytes.len() as u32,
                1,
                ptr::null_mut(),
                None,
            );
            let count = hb::hb_face_count(blob);
            if data.index >= count {
                hb::hb_blob_destroy(blob);
                anyhow::bail!("Invalid SFNT font or face index: {}", run.font);
            }
            let face = hb::hb_face_create(blob, data.index);
            hb::hb_blob_destroy(blob);
            let glyph_count = hb::hb_face_get_glyph_count(face);
            let upem = hb::hb_face_get_upem(face) as i32;
            let raw = hb::hb_font_create(face);
            hb::hb_face_destroy(face);
            let font = Self {
                raw,
                _bytes: data.bytes.clone(),
            };
            ensure!(glyph_count > 0 && upem > 0, "Invalid font: {}", run.font);
            hb::hb_ot_font_set_funcs(raw);
            hb::hb_font_set_scale(raw, upem, upem);
            let variations: Vec<_> = run
                .variations
                .iter()
                .map(|(key, value)| hb::Variation {
                    tag: tag(key),
                    value: *value,
                })
                .collect();
            hb::hb_font_set_variations(raw, variations.as_ptr(), variations.len() as u32);
            Ok(font)
        }
    }
}

impl Drop for Font {
    fn drop(&mut self) {
        // SAFETY: rawの所有者はこのFontのみ
        unsafe { hb::hb_font_destroy(self.raw) }
    }
}

struct Buffer(*mut hb::Handle);
impl Buffer {
    fn new() -> Self {
        // SAFETY: HarfBuzzが所有ハンドルを返す
        Self(unsafe { hb::hb_buffer_create() })
    }
}
impl Drop for Buffer {
    fn drop(&mut self) {
        // SAFETY: ハンドルを一度だけ解放
        unsafe { hb::hb_buffer_destroy(self.0) }
    }
}

struct Plan(*mut hb::Handle);
impl Drop for Plan {
    fn drop(&mut self) {
        // SAFETY: ハンドルを一度だけ解放
        unsafe { hb::hb_shape_plan_destroy(self.0) }
    }
}

pub struct Run<'a> {
    font: &'a Font,
    input: &'a corpus::Run,
    text_with_context: String,
    props: hb::Properties,
    features: Vec<hb::Feature>,
    buffer: Buffer,
    plan: Option<Plan>,
    policy: PlanMode,
}

impl<'a> Run<'a> {
    pub fn new(font: &'a Font, input: &'a corpus::Run, policy: PlanMode) -> Result<Self> {
        let direction = match input.direction {
            corpus::Direction::Ltr => 4,
            corpus::Direction::Rtl => 5,
            corpus::Direction::Ttb => 6,
            corpus::Direction::Btt => 7,
        };
        // SAFETY: 検証済みUTF-8文字列と実際のバイト長
        let language = unsafe {
            hb::hb_language_from_string(input.language.as_ptr().cast(), input.language.len() as i32)
        };
        // SAFETY: 検証済みの4文字タグ
        let script = unsafe { hb::hb_script_from_string(input.script.as_ptr().cast(), 4) };
        let mut run = Self {
            font,
            input,
            text_with_context: format!("{}{}{}", input.pre_context, input.text, input.post_context),
            props: hb::Properties {
                direction,
                script,
                language,
                reserved1: ptr::null_mut(),
                reserved2: ptr::null_mut(),
            },
            features: input
                .features
                .iter()
                .map(|f| hb::Feature {
                    tag: tag(&f.tag),
                    value: f.value,
                    start: f.start,
                    end: f.end,
                })
                .collect(),
            buffer: Buffer::new(),
            plan: None,
            policy,
        };
        if policy == PlanMode::Cached {
            run.plan = Some(run.create_plan());
        }
        Ok(run)
    }

    fn create_plan(&self) -> Plan {
        // SAFETY: faceと座標はfontが保持 参照先は呼び出し中に有効
        unsafe {
            let mut count = 0;
            let coords = hb::hb_font_get_var_coords_normalized(self.font.raw, &mut count);
            let shapers = [c"ot".as_ptr(), ptr::null()];
            Plan(hb::hb_shape_plan_create2(
                hb::hb_font_get_face(self.font.raw),
                &self.props,
                self.features.as_ptr(),
                self.features.len() as u32,
                coords,
                count,
                shapers.as_ptr(),
            ))
        }
    }
}

impl RunEngine for Run<'_> {
    fn shape(&mut self, fresh_buffer: bool) -> Result<usize> {
        self.prepare_buffer(fresh_buffer)?;
        self.execute()
    }

    fn prepare_buffer(&mut self, fresh_buffer: bool) -> Result<()> {
        if fresh_buffer {
            self.buffer = Buffer::new();
        }
        // SAFETY: バッファは排他的に使用 入力とfeaturesは検証済み
        unsafe {
            let buffer = self.buffer.0;
            hb::hb_buffer_reset(buffer);
            hb::hb_buffer_set_segment_properties(buffer, &self.props);
            hb::hb_buffer_set_cluster_level(buffer, self.input.cluster_level as u32);
            hb::hb_buffer_set_flags(buffer, self.input.flag_bits());
            let prefix = self.input.pre_context.len() as u32;
            hb::hb_buffer_add_utf8(
                buffer,
                self.text_with_context.as_ptr().cast(),
                self.text_with_context.len() as i32,
                prefix,
                self.input.text.len() as i32,
            );
            if prefix > 0 {
                let mut len = 0;
                let infos = hb::hb_buffer_get_glyph_infos(buffer, &mut len);
                for index in 0..len as usize {
                    (*infos.add(index)).cluster -= prefix;
                }
            }
        }
        Ok(())
    }

    fn measure_plan(&self) {
        drop(std::hint::black_box(self.create_plan()));
    }

    fn execute(&mut self) -> Result<usize> {
        // SAFETY: prepare_buffer済みの排他的バッファと有効なプラン
        unsafe {
            let buffer = self.buffer.0;
            let rebuilt = (self.policy == PlanMode::Rebuild).then(|| self.create_plan());
            let plan = self.plan.as_ref().or(rebuilt.as_ref());
            let ok = if let Some(plan) = plan {
                hb::hb_shape_plan_execute(
                    plan.0,
                    self.font.raw,
                    buffer,
                    self.features.as_ptr(),
                    self.features.len() as u32,
                )
            } else {
                let shapers = [c"ot".as_ptr(), ptr::null()];
                hb::hb_shape_full(
                    self.font.raw,
                    buffer,
                    self.features.as_ptr(),
                    self.features.len() as u32,
                    shapers.as_ptr(),
                )
            };
            ensure!(
                ok != 0 && hb::hb_buffer_allocation_successful(buffer) != 0,
                "HarfBuzz shaping failed"
            );
            Ok(hb::hb_buffer_get_length(buffer) as usize)
        }
    }

    fn glyphs(&self) -> Vec<Glyph> {
        // SAFETY: shape後のバッファを読み取り コピー中の変更なし
        unsafe {
            let mut len = 0;
            let infos = hb::hb_buffer_get_glyph_infos(self.buffer.0, &mut len);
            let positions = hb::hb_buffer_get_glyph_positions(self.buffer.0, ptr::null_mut());
            (0..len as usize)
                .map(|index| {
                    let i = &*infos.add(index);
                    let p = &*positions.add(index);
                    Glyph {
                        id: i.codepoint,
                        cluster: i.cluster,
                        x_advance: p.x_advance,
                        y_advance: p.y_advance,
                        x_offset: p.x_offset,
                        y_offset: p.y_offset,
                        flags: i.mask & 7,
                    }
                })
                .collect()
        }
    }

    fn trace(&mut self) -> Result<()> {
        // SAFETY: callbackは静的関数 user_data不使用
        unsafe {
            hb::hb_buffer_set_message_func(
                self.buffer.0,
                Some(trace_message),
                ptr::null_mut(),
                None,
            )
        }
        Ok(())
    }
}

unsafe extern "C" fn trace_message(
    _: *mut hb::Handle,
    _: *mut hb::Handle,
    message: *const c_char,
    _: *mut c_void,
) -> i32 {
    // SAFETY: HarfBuzzが呼び出し中に有効なNUL終端文字列を渡す
    let message = unsafe { CStr::from_ptr(message) }.to_string_lossy();
    let _ = writeln!(std::io::stderr().lock(), "{message}");
    1
}

pub fn version() -> String {
    // SAFETY: HarfBuzzの静的NUL終端文字列
    unsafe { CStr::from_ptr(hb::hb_version_string()) }
        .to_string_lossy()
        .into_owned()
}
