//! OpenType layout.

use alloc::{
    boxed::Box,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::ops::{Index, IndexMut};
use core::sync::atomic::{AtomicU64, Ordering};
use read_fonts::model::Tables;

use super::apply::ApplyContext;
use super::buffer::*;
use super::set_digest::SetDigest;
use super::GlyphInfo;
use crate::aat::AatData;
use crate::ot::apply::check_glyph_property;
use crate::ot::lookup::CachedLookup;
use crate::ot::map::LookupMap;
use crate::ot::OtData;
use crate::plan::ShapePlan;
use crate::unicode::{space_fallback, GeneralCategory};
use crate::BufferFlags;
use crate::ShaperFont;

impl GlyphInfo {
    declare_buffer_var!(u16, 1, 0, GLYPH_PROPS_VAR, glyph_props, set_glyph_props);
    declare_buffer_var!(u8, 1, 2, LIG_PROPS_VAR, lig_props, set_lig_props);
    declare_buffer_var!(u8, 1, 3, SYLLABLE_VAR, syllable, set_syllable);
    declare_buffer_var!(
        u16,
        2,
        0,
        UNICODE_PROPS_VAR,
        unicode_props,
        set_unicode_props
    );
}

impl Buffer {
    pub(crate) fn allocate_unicode_vars(&mut self) {
        self.allocate_var(GlyphInfo::UNICODE_PROPS_VAR);
    }
    pub(crate) fn deallocate_unicode_vars(&mut self) {
        self.deallocate_var(GlyphInfo::UNICODE_PROPS_VAR);
    }
    pub(crate) fn assert_unicode_vars(&mut self) {
        self.assert_var(GlyphInfo::UNICODE_PROPS_VAR);
    }

    pub(crate) fn allocate_gsubgpos_vars(&mut self) {
        self.allocate_var(GlyphInfo::LIG_PROPS_VAR);
        self.allocate_var(GlyphInfo::GLYPH_PROPS_VAR);
    }
    pub(crate) fn deallocate_gsubgpos_vars(&mut self) {
        self.deallocate_var(GlyphInfo::LIG_PROPS_VAR);
        self.deallocate_var(GlyphInfo::GLYPH_PROPS_VAR);
    }
    pub(crate) fn assert_gsubgpos_vars(&mut self) {
        self.assert_var(GlyphInfo::LIG_PROPS_VAR);
        self.assert_var(GlyphInfo::GLYPH_PROPS_VAR);
    }
}

pub const MAX_NESTING_LEVEL: usize = 64;
pub const MAX_CONTEXT_LENGTH: usize = 64;
pub const MAX_SYLLABLE_LENGTH: usize = 64;

pub fn has_kerning(aat: &AatData) -> bool {
    aat.kern.is_some()
}

pub fn has_machine_kerning(aat: &AatData) -> bool {
    match aat.kern {
        Some(ref kern) => kern
            .subtables()
            .filter_map(Result::ok)
            .any(|s| s.is_state_machine()),
        None => false,
    }
}

pub fn has_cross_kerning(aat: &AatData) -> bool {
    match aat.kern {
        Some(ref kern) => kern
            .subtables()
            .filter_map(Result::ok)
            .any(|s| s.is_cross_stream()),
        None => false,
    }
}

// apply_kern

pub fn set_glyph_props<const SET_DIGEST: bool>(ot: &OtData, buffer: &mut Buffer) {
    buffer.assert_gsubgpos_vars();

    let len = buffer.len;
    for info in buffer.info.get_mut(..len).unwrap_or_default() {
        if SET_DIGEST {
            buffer.digest.add(info.glyph_id);
        }
        info.set_glyph_props(ot.glyph_props(info.as_glyph()));
        info.set_lig_props(0);
    }
}

pub fn has_glyph_classes(ot: &OtData) -> bool {
    ot.has_glyph_classes()
}

// get_gsubgpos_table

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutTableKind {
    Gsub = 0,
    Gpos = 1,
}

impl LayoutTableKind {
    pub fn iter() -> impl Iterator<Item = LayoutTableKind> {
        [Self::Gsub, Self::Gpos].iter().copied()
    }
}

impl<T> Index<LayoutTableKind> for [T] {
    type Output = T;

    fn index(&self, table_index: LayoutTableKind) -> &Self::Output {
        &self[table_index as usize]
    }
}

impl<T> IndexMut<LayoutTableKind> for [T] {
    fn index_mut(&mut self, table_index: LayoutTableKind) -> &mut Self::Output {
        &mut self[table_index as usize]
    }
}

/// A lookup-based layout table (GSUB or GPOS).
pub trait LayoutTable {
    /// The kind of layout table.
    const KIND: LayoutTableKind;

    /// Whether lookups in this table can be applied to the buffer in-place.
    const IN_PLACE: bool;

    /// Get the lookup at the specified index.
    fn get_lookup(&self, index: u16) -> Option<&CachedLookup<'_>>;

    /// The shared font tables that own these lookups.
    fn tables(&self) -> Option<&Arc<Tables>>;
}

const MAX_INDEXED_LOOKUPS: usize = 64;
const MIN_INDEXED_GLYPHS: usize = 16;
const MIN_POSITION_GLYPHS: usize = 32;
const MAX_POSITION_WORDS: usize = 8192;
const NO_COLUMN: u16 = u16::MAX;

pub(crate) struct CandidateIndex {
    pub words: Vec<u64>,
    pub added: Option<SetDigest>,
    columns: Vec<u16>,
    glyphs: Vec<u32>,
    dirty: u64,
    len: usize,
    active: bool,
}

impl CandidateIndex {
    pub fn new() -> Self {
        Self {
            words: Vec::new(),
            added: None,
            columns: Vec::new(),
            glyphs: Vec::new(),
            dirty: 0,
            len: 0,
            active: false,
        }
    }

    fn begin(&mut self, infos: &[GlyphInfo], columns: [u16; MAX_INDEXED_LOOKUPS]) {
        self.len = infos.len();
        self.columns.clear();
        self.columns.extend_from_slice(&columns);
        self.glyphs.resize(64, u32::MAX);
        self.words
            .reserve(MAX_INDEXED_LOOKUPS.saturating_sub(self.words.len()));
        for (glyph, info) in self.glyphs.iter_mut().zip(infos) {
            *glyph = info.glyph_id;
        }
        self.dirty = 0;
        self.active = true;
    }

    pub fn stop(&mut self) {
        self.active = false;
        self.dirty = 0;
        self.added = None;
    }

    #[inline]
    pub fn replace(&mut self, at: usize) {
        if self.active {
            if at < self.len {
                self.dirty |= 1u64 << at;
            } else {
                self.stop();
            }
        }
    }

    #[inline]
    pub fn splice(&mut self, at: usize, removed: usize, inserted: usize) {
        if !self.active {
            return;
        }
        if removed == 1 && inserted == 1 {
            self.replace(at);
            return;
        }
        let Some(end) = at.checked_add(removed).filter(|&end| end <= self.len) else {
            self.stop();
            return;
        };
        let Some(len) = (self.len - removed)
            .checked_add(inserted)
            .filter(|&len| len <= 64)
        else {
            self.stop();
            return;
        };
        let prefix = low_bits(at);
        let new_end = at + inserted;
        let shift = |bits: u64| {
            (bits & prefix)
                | bits
                    .checked_shr(end as u32)
                    .unwrap_or(0)
                    .checked_shl(new_end as u32)
                    .unwrap_or(0)
        };
        for bits in &mut self.words {
            *bits = shift(*bits);
        }
        self.dirty = shift(self.dirty) | low_bits(inserted).checked_shl(at as u32).unwrap_or(0);
        self.glyphs.copy_within(end..self.len, new_end);
        self.glyphs[at..new_end].fill(u32::MAX);
        self.len = len;
    }
}

fn low_bits(len: usize) -> u64 {
    u64::MAX.checked_shr((64 - len) as u32).unwrap_or(0)
}

pub(crate) struct GlyphDispatch {
    tables: Weak<Tables>,
    rows: crate::once::Once<Box<DispatchRows>>,
}

struct DispatchRows {
    rows: [AtomicU64; 256],
    high: Option<Box<[AtomicU64; 256]>>,
}

impl GlyphDispatch {
    fn new(tables: &Arc<Tables>) -> Self {
        Self {
            tables: Arc::downgrade(tables),
            rows: crate::once::Once::new(),
        }
    }
}

impl DispatchRows {
    fn new(lookups: usize) -> Box<Self> {
        Box::new(Self {
            rows: core::array::from_fn(|_| AtomicU64::new(u64::MAX)),
            high: (lookups > 32)
                .then(|| Box::new(core::array::from_fn(|_| AtomicU64::new(u64::MAX)))),
        })
    }

    #[inline]
    fn candidates<T: LayoutTable>(&self, glyph: u32, maps: &[LookupMap], table: &T) -> u64 {
        let index = (glyph.wrapping_mul(0x9E37_79B9) >> 24) as usize;
        let row = self.rows[index].load(Ordering::Relaxed);
        if row != u64::MAX && (row >> 32) as u32 == glyph {
            let low = u64::from(row as u32);
            if let Some(high) = &self.high {
                let row = high[index].load(Ordering::Relaxed);
                if row == u64::MAX || (row >> 32) as u32 != glyph {
                    return self.candidates_uncached(glyph, maps, table, index);
                }
                low | (u64::from(row as u32) << 32)
            } else {
                low
            }
        } else {
            self.candidates_uncached(glyph, maps, table, index)
        }
    }

    #[inline(never)]
    fn candidates_uncached<T: LayoutTable>(
        &self,
        glyph: u32,
        maps: &[LookupMap],
        table: &T,
        index: usize,
    ) -> u64 {
        let mut target = index;
        for probe in 0..4 {
            let slot = (index + probe) & 255;
            let row = self.rows[slot].load(Ordering::Relaxed);
            if row == u64::MAX {
                target = slot;
                break;
            }
            if (row >> 32) as u32 != glyph {
                continue;
            }
            let low = u64::from(row as u32);
            let Some(high) = &self.high else { return low };
            let row = high[slot].load(Ordering::Relaxed);
            if row != u64::MAX && (row >> 32) as u32 == glyph {
                return low | (u64::from(row as u32) << 32);
            }
            target = slot;
            break;
        }
        let mut bits = 0u64;
        for (slot, map) in maps.iter().enumerate() {
            if table
                .get_lookup(map.index)
                .is_some_and(|lookup| lookup.covers(glyph))
            {
                bits |= 1 << slot;
            }
        }
        self.rows[target].store(
            (u64::from(glyph) << 32) | u64::from(bits as u32),
            Ordering::Relaxed,
        );
        if let Some(high) = &self.high {
            high[target].store((u64::from(glyph) << 32) | (bits >> 32), Ordering::Relaxed);
        }
        bits
    }
}

fn build_positions<T: LayoutTable>(
    dispatch: &DispatchRows,
    maps: &[LookupMap],
    table: &T,
    infos: &[GlyphInfo],
    positions: &mut Vec<u64>,
    allowed: u64,
) -> Option<[u16; MAX_INDEXED_LOOKUPS]> {
    let words = infos.len().div_ceil(64);
    let size = words.saturating_mul(allowed.count_ones() as usize);
    if infos.len() < 4 || size > MAX_POSITION_WORDS {
        return None;
    }
    let mut sample = [0u64; 8];
    let sample_len = infos.len().min(sample.len());
    let mut hits = 0;
    for (i, info) in infos[..sample_len].iter().enumerate() {
        sample[i] = dispatch.candidates(info.glyph_id, maps, table) & allowed;
        hits += sample[i].count_ones() as usize;
    }
    if hits * 2 >= sample_len * allowed.count_ones() as usize {
        return None;
    }
    positions.clear();
    let mut columns = [NO_COLUMN; MAX_INDEXED_LOOKUPS];
    for (i, info) in infos.iter().enumerate() {
        let mut bits = if i < sample_len {
            sample[i]
        } else {
            dispatch.candidates(info.glyph_id, maps, table) & allowed
        };
        let bit = 1u64 << (i & 63);
        let word = i / 64;
        while bits != 0 {
            let slot = bits.trailing_zeros() as usize;
            let column = &mut columns[slot];
            if *column == NO_COLUMN {
                *column = positions.len() as u16;
                let size = positions.len() + words;
                if size > positions.capacity() {
                    positions.reserve_exact(
                        size.next_power_of_two().min(MAX_POSITION_WORDS) - positions.len(),
                    );
                }
                positions.resize(size, 0);
            }
            positions[usize::from(*column) + word] |= bit;
            bits &= bits - 1;
        }
    }
    Some(columns)
}

/// Called before substitution lookups are performed, to ensure that glyph
/// class and other properties are set on the glyphs in the buffer.
pub fn substitute_start(ot: &OtData, buffer: &mut Buffer) {
    set_glyph_props::<false>(ot, buffer);
}

pub fn substitute_start_with_digest(ot: &OtData, buffer: &mut Buffer) {
    buffer.digest = SetDigest::new();
    set_glyph_props::<true>(ot, buffer);
}

/// Applies the lookups in the given GSUB or GPOS table.
#[inline]
pub fn apply_layout_table<T: LayoutTable>(
    plan: &ShapePlan,
    font: &ShaperFont<'_, '_>,
    buffer: &mut Buffer,
    table: Option<&T>,
) {
    if cfg!(feature = "trace") || buffer.len < MIN_INDEXED_GLYPHS {
        apply_layout::<T, false>(plan, font, buffer, table);
    } else {
        apply_layout::<T, true>(plan, font, buffer, table);
    }
}

fn apply_layout<T: LayoutTable, const INDEXED: bool>(
    plan: &ShapePlan,
    font: &ShaperFont<'_, '_>,
    buffer: &mut Buffer,
    table: Option<&T>,
) {
    let mut ctx = ApplyContext::new(
        T::KIND,
        font.layout(),
        font.scale,
        buffer,
        #[cfg(feature = "trace")]
        font,
    );
    let mut positions = if INDEXED {
        core::mem::take(&mut ctx.buffer.candidates.words)
    } else {
        Vec::new()
    };

    for (stage_index, stage) in plan.ot_map.stages(T::KIND).iter().enumerate() {
        let maps = plan.ot_map.stage_lookups(T::KIND, stage_index);
        if let Some(table) = table.filter(|_| !maps.is_empty()) {
            let eligible = INDEXED && (3..=MAX_INDEXED_LOOKUPS).contains(&maps.len());
            let domain = if eligible {
                stage.dispatch.get().filter(|dispatch| {
                    table.tables().is_some_and(|tables| {
                        core::ptr::eq(Arc::as_ptr(tables), dispatch.tables.as_ptr())
                    })
                })
            } else {
                None
            };
            let active = if T::IN_PLACE && domain.is_some() {
                let mut active = 0u64;
                let unsafe_concat = ctx
                    .buffer
                    .flags
                    .contains(BufferFlags::PRODUCE_UNSAFE_TO_CONCAT);
                for (slot, map) in maps.iter().enumerate() {
                    if map.mask != 0
                        && table.get_lookup(map.index).is_some_and(|lookup| {
                            lookup.digest().may_intersect(&ctx.buffer.digest)
                                && (unsafe_concat
                                    || lookup.digest_second().may_intersect(&ctx.buffer.digest))
                        })
                    {
                        active |= 1u64 << slot;
                    }
                }
                Some(active)
            } else {
                None
            };
            let dispatch = domain
                .filter(|_| active.is_none_or(|active| active.count_ones() >= 3))
                .map(|dispatch| {
                    dispatch
                        .rows
                        .get_or_init(|| DispatchRows::new(maps.len()))
                        .as_ref()
                });
            let words = ctx.buffer.len.div_ceil(64);
            let columns = dispatch.and_then(|dispatch| {
                if ctx.buffer.successful
                    && ctx.buffer.len >= MIN_POSITION_GLYPHS
                    && (T::IN_PLACE || ctx.buffer.len <= 64)
                {
                    build_positions(
                        dispatch,
                        maps,
                        table,
                        ctx.buffer.glyph_infos(),
                        &mut positions,
                        active.unwrap_or_else(|| low_bits(maps.len())),
                    )
                } else {
                    None
                }
            });
            let indexed = columns.is_some();
            let empty = indexed && positions.is_empty();
            let present =
                if ctx.buffer.len < MIN_POSITION_GLYPHS || (!T::IN_PLACE && ctx.buffer.len > 64) {
                    dispatch.and_then(|dispatch| {
                        let mut present = 0u64;
                        let all = active.unwrap_or_else(|| low_bits(maps.len()));
                        for info in ctx.buffer.glyph_infos() {
                            present |= dispatch.candidates(info.glyph_id, maps, table) & all;
                            if present == all {
                                return None;
                            }
                        }
                        if !T::IN_PLACE {
                            ctx.buffer.candidates.added = Some(SetDigest::new());
                        }
                        Some(present)
                    })
                } else {
                    None
                };
            if indexed && !empty && !T::IN_PLACE {
                ctx.buffer.candidates.words = core::mem::take(&mut positions);
                ctx.buffer
                    .candidates
                    .begin(&ctx.buffer.info[..ctx.buffer.len], columns.unwrap());
            }
            if empty {
                ctx.buffer.idx = if T::IN_PLACE { ctx.buffer.len } else { 0 };
            }
            let apply_maps = if empty { &[][..] } else { maps };
            for (slot, lookup_map) in apply_maps.iter().enumerate() {
                if present == Some(0)
                    || (T::IN_PLACE && present.is_some_and(|present| present & (1u64 << slot) == 0))
                {
                    if T::IN_PLACE {
                        ctx.buffer.idx = ctx.buffer.len;
                    }
                    continue;
                }
                if active.is_some_and(|active| active & (1u64 << slot) == 0) {
                    continue;
                }
                if indexed {
                    let absent = if T::IN_PLACE {
                        columns.as_ref().unwrap()[slot] == NO_COLUMN
                    } else {
                        ctx.buffer.candidates.active
                            && ctx.buffer.candidates.columns[slot] == NO_COLUMN
                    };
                    if absent {
                        if T::IN_PLACE {
                            ctx.buffer.idx = ctx.buffer.len;
                        }
                        continue;
                    }
                }
                let Some(lookup) = table.get_lookup(lookup_map.index) else {
                    continue;
                };

                message_continue!(
                    ctx,
                    "start lookup {} feature '{}'",
                    lookup_map.index,
                    crate::ot::map::MessageTag(Some(lookup_map.feature_tag))
                );

                if present.is_none_or(|present| {
                    present & (1 << slot) != 0
                        || ctx
                            .buffer
                            .candidates
                            .added
                            .as_ref()
                            .is_some_and(|added| lookup.digest().may_intersect(added))
                }) && (active.is_some()
                    || (lookup.digest().may_intersect(&ctx.buffer.digest)
                        && (ctx
                            .buffer
                            .flags
                            .contains(BufferFlags::PRODUCE_UNSAFE_TO_CONCAT)
                            || lookup.digest_second().may_intersect(&ctx.buffer.digest))))
                {
                    ctx.lookup_index = lookup_map.index;
                    ctx.set_lookup_mask(lookup_map.mask);
                    ctx.auto_zwj = lookup_map.auto_zwj;
                    ctx.auto_zwnj = lookup_map.auto_zwnj;

                    ctx.random = lookup_map.random;
                    ctx.per_syllable = lookup_map.per_syllable;

                    if indexed && T::IN_PLACE {
                        let start = usize::from(columns.as_ref().unwrap()[slot]);
                        apply_positions(&mut ctx, lookup, &positions[start..start + words]);
                    } else if indexed && ctx.buffer.candidates.active {
                        let dispatch = dispatch.unwrap();
                        if lookup.is_reverse() {
                            apply_string::<T>(&mut ctx, lookup);
                            repair_positions(&mut ctx, dispatch, maps, table);
                        } else {
                            apply_short_positions(&mut ctx, lookup, slot, dispatch, maps, table);
                        }
                    } else {
                        apply_string::<T>(&mut ctx, lookup);
                    }
                } else {
                    message!(
                        ctx,
                        "skipped lookup {} feature '{}' because no glyph matches",
                        lookup_map.index,
                        crate::ot::map::MessageTag(Some(lookup_map.feature_tag))
                    );
                }
                message!(
                    ctx,
                    "end lookup {} feature '{}'",
                    lookup_map.index,
                    crate::ot::map::MessageTag(Some(lookup_map.feature_tag))
                );
            }
            if indexed && !empty && !T::IN_PLACE {
                ctx.buffer.candidates.stop();
                positions = core::mem::take(&mut ctx.buffer.candidates.words);
            }
            if INDEXED {
                ctx.buffer.candidates.added = None;
            }
            // Initialize after the first pass to preserve lazy parsing order.
            if eligible && stage.dispatch.get().is_none() {
                if let Some(tables) = table.tables() {
                    stage.dispatch.get_or_init(|| GlyphDispatch::new(tables));
                }
            }
        }

        if let Some(func) = stage.pause_func {
            let changed = func(plan, font, ctx.buffer);
            if changed.is_none() {
                ctx.buffer.index_failed();
            }
            // A pause that stopped early may still have changed the buffer.
            if changed.unwrap_or(true) {
                ctx.buffer.update_digest();
            }
        }
    }
    if INDEXED {
        ctx.buffer.candidates.words = positions;
    }
}

fn repair_positions<T: LayoutTable>(
    ctx: &mut ApplyContext,
    dispatch: &DispatchRows,
    maps: &[LookupMap],
    table: &T,
) {
    let mut dirty = ctx.buffer.candidates.dirty;
    while dirty != 0 && ctx.buffer.candidates.active {
        let index = dirty.trailing_zeros() as usize;
        dirty &= dirty - 1;
        let info = if !ctx.buffer.have_output {
            ctx.buffer.glyph_infos().get(index)
        } else if index < ctx.buffer.out_len {
            ctx.buffer.out_info().get(index)
        } else {
            let input = index - ctx.buffer.backtrack_len() + ctx.buffer.idx;
            ctx.buffer.glyph_infos().get(input)
        };
        let Some(glyph) = info.map(|info| info.glyph_id) else {
            ctx.buffer.candidates.stop();
            return;
        };
        let old = ctx.buffer.candidates.glyphs[index];
        let old_bits = if old == u32::MAX {
            0
        } else {
            dispatch.candidates(old, maps, table)
        };
        let new_bits = dispatch.candidates(glyph, maps, table);
        let mut changed = old_bits ^ new_bits;
        while changed != 0 {
            let slot = changed.trailing_zeros() as usize;
            changed &= changed - 1;
            let mut column = ctx.buffer.candidates.columns[slot];
            if column == NO_COLUMN {
                column = ctx.buffer.candidates.words.len() as u16;
                ctx.buffer.candidates.words.push(0);
                ctx.buffer.candidates.columns[slot] = column;
            }
            ctx.buffer.candidates.words[usize::from(column)] ^= 1u64 << index;
        }
        ctx.buffer.candidates.glyphs[index] = glyph;
    }
    ctx.buffer.candidates.dirty = 0;
}

fn apply_short_positions<T: LayoutTable>(
    ctx: &mut ApplyContext,
    lookup: &CachedLookup,
    slot: usize,
    dispatch: &DispatchRows,
    maps: &[LookupMap],
    table: &T,
) -> bool {
    let column = usize::from(ctx.buffer.candidates.columns[slot]);
    if ctx.buffer.candidates.words[column] == 0 || ctx.lookup_mask() == 0 {
        return false;
    }
    ctx.lookup_props = lookup.props();
    ctx.update_matchers();
    ctx.buffer.clear_output();
    let mut ret = false;
    let ot = ctx.layout.ot;
    let mask = ctx.lookup_mask();
    let props = ctx.lookup_props;
    while ctx.buffer.successful {
        let bits = ctx.buffer.candidates.words[column]
            & u64::MAX.checked_shl(ctx.buffer.out_len as u32).unwrap_or(0);
        if bits == 0 {
            break;
        }
        let index = bits.trailing_zeros() as usize;
        ctx.buffer.next_glyphs(index - ctx.buffer.out_len);
        let Some(info) = ctx.buffer.cur(0) else { break };
        if info.mask & mask != 0 && check_glyph_property(ot, info, props) {
            if lookup.apply(ctx, false).is_some() {
                ret = true;
            } else {
                ctx.buffer.next_glyph();
            }
            repair_positions(ctx, dispatch, maps, table);
            if !ctx.buffer.candidates.active {
                ret |= scan_forward(ctx, lookup, false);
                break;
            }
        } else {
            ctx.buffer.next_glyph();
        }
    }
    ctx.buffer.sync();
    ret
}

fn apply_positions(ctx: &mut ApplyContext, lookup: &CachedLookup, positions: &[u64]) -> bool {
    let mut ret = false;
    ctx.lookup_props = lookup.props();
    ctx.update_matchers();
    ctx.buffer.idx = 0;
    let ot = ctx.layout.ot;
    let lookup_mask = ctx.lookup_mask();
    let lookup_props = ctx.lookup_props;
    for (word, &bits) in positions.iter().enumerate() {
        let mut bits = bits;
        while bits != 0 && ctx.buffer.successful {
            let index = word * 64 + bits.trailing_zeros() as usize;
            bits &= bits - 1;
            if index < ctx.buffer.idx {
                continue;
            }
            ctx.buffer.idx = index;
            let Some(info) = ctx.buffer.glyph_infos().get(index) else {
                break;
            };
            if info.mask & lookup_mask == 0 || !check_glyph_property(ot, info, lookup_props) {
                continue;
            }
            if lookup.apply(ctx, false).is_some() {
                ret = true;
            } else {
                ctx.buffer.next_glyph();
            }
        }
        if !ctx.buffer.successful {
            break;
        }
    }
    if ctx.buffer.successful {
        ctx.buffer.idx = ctx.buffer.len;
    }
    ret
}

fn apply_string<T: LayoutTable>(ctx: &mut ApplyContext, lookup: &CachedLookup) -> bool {
    if ctx.buffer.is_empty() || ctx.lookup_mask() == 0 {
        return false;
    }

    let ret;
    ctx.lookup_props = lookup.props();
    ctx.update_matchers();

    if !lookup.is_reverse() {
        // in/out forward substitution/positioning
        if !T::IN_PLACE {
            ctx.buffer.clear_output();
        }
        ctx.buffer.idx = 0;
        ret = apply_forward(ctx, lookup);

        if !T::IN_PLACE {
            ctx.buffer.sync();
        }
    } else {
        // in-place backward substitution/positioning
        debug_assert!(!ctx.buffer.have_output);

        ctx.buffer.idx = ctx.buffer.len - 1;
        ret = apply_backward(ctx, lookup);
    }
    ret
}

fn apply_forward(ctx: &mut ApplyContext, lookup: &CachedLookup) -> bool {
    let use_hot_subtable_cache = lookup.cache_enter(ctx);
    let ret = scan_forward(ctx, lookup, use_hot_subtable_cache);
    if use_hot_subtable_cache {
        lookup.cache_leave(ctx);
    }
    ret
}

#[inline(always)]
fn scan_forward(
    ctx: &mut ApplyContext,
    lookup: &CachedLookup,
    use_hot_subtable_cache: bool,
) -> bool {
    let mut ret = false;

    // Loop-invariant: nested lookups save and restore lookup_props and the
    // lookup mask (see ApplyContext::recurse), so these can be
    // hoisted, letting the candidate scan below run over a slice without
    // per-glyph bounds checks.
    let ot = ctx.layout.ot;
    let lookup_mask = ctx.lookup_mask();
    let lookup_props = ctx.lookup_props;

    while ctx.buffer.successful {
        let idx = ctx.buffer.idx;
        let infos = ctx.buffer.glyph_infos();
        let mut j = idx;
        while let Some(info) = infos.get(j) {
            if lookup.digest.may_have(info.glyph_id)
                && (info.mask & lookup_mask) != 0
                && check_glyph_property(ot, info, lookup_props)
            {
                break;
            }
            j += 1;
        }

        if j > idx {
            ctx.buffer.next_glyphs(j - idx);
        }
        if ctx.buffer.idx >= ctx.buffer.len {
            break;
        }

        if lookup.apply(ctx, use_hot_subtable_cache).is_some() {
            ret = true;
        } else {
            ctx.buffer.next_glyph();
        }
    }

    ret
}

pub(crate) fn apply_synthesized_subst_lookup(ctx: &mut ApplyContext, lookup: &CachedLookup) {
    if ctx.buffer.is_empty() || ctx.lookup_mask() == 0 {
        return;
    }

    debug_assert!(lookup.is_subst);
    debug_assert!(!lookup.is_reverse());
    ctx.lookup_props = lookup.props();
    ctx.update_matchers();
    ctx.buffer.clear_output();
    ctx.buffer.idx = 0;
    apply_forward(ctx, lookup);
    ctx.buffer.sync();
}

fn apply_backward(ctx: &mut ApplyContext, lookup: &CachedLookup) -> bool {
    let mut ret = false;
    // Loop-invariant hoisting as in apply_forward; reverse lookups don't
    // recurse, and in-place application cannot change the buffer length.
    let ot = ctx.layout.ot;
    let lookup_mask = ctx.lookup_mask();
    let lookup_props = ctx.lookup_props;

    loop {
        let candidate = ctx
            .buffer
            .info
            .get(..=ctx.buffer.idx)
            .unwrap_or_default()
            .iter()
            .rposition(|info| {
                lookup.digest.may_have(info.glyph_id)
                    && (info.mask & lookup_mask) != 0
                    && check_glyph_property(ot, info, lookup_props)
            });

        let Some(idx) = candidate else {
            ctx.buffer.idx = 0;
            break;
        };

        ctx.buffer.idx = idx;
        ret |= lookup.apply(ctx, false).is_some();

        if idx == 0 {
            break;
        }
        ctx.buffer.idx = idx - 1;
    }
    ret
}

/* unicode_props */

/* Design:
 * unicode_props() is a two-byte number.  The low byte includes:
 * - Modified General_Category: 5 bits.
 * - A bit each for:
 *   * Is it Default_Ignorable(); we have a modified Default_Ignorable().
 *   * Whether it's one of the four Mongolian Free Variation Selectors,
 *     CGJ, or other characters that are hidden but should not be ignored
 *     like most other Default_Ignorable()s do during GSUB matching.
 *   * Whether it's a grapheme continuation.
 *
 * The high-byte has different meanings, switched by the Gen-Cat:
 * - For Mn,Mc,Me: the modified Combining_Class.
 * - For Cf: whether it's ZWJ, ZWNJ, or something else.
 * - For Ws: index of which space character this is, if space fallback
 *   is needed, ie. we don't set this by default, only if asked to.
 *
 * Above I said "modified" General_Category. This is because we need to
 * remember Variation Selectors, and we don't have bits left. So we
 * change their Gen_Cat from Mn to Cf, and use a bit of the high byte to
 * remember them.
 */

//  enum hb_unicode_props_flags_t {
//     UPROPS_MASK_GEN_CAT	= 0x001Fu,
//     UPROPS_MASK_IGNORABLE	= 0x0020u,
//     UPROPS_MASK_HIDDEN	= 0x0040u, /* MONGOLIAN FREE VARIATION SELECTOR 1..4, or TAG characters */
//     UPROPS_MASK_CONTINUATION=0x0080u,

//     /* If GEN_CAT=FORMAT, top byte masks: */
//     UPROPS_MASK_Cf_ZWJ	= 0x0100u,
//     UPROPS_MASK_Cf_ZWNJ	= 0x0200u
//   };
//   HB_MARK_AS_FLAG_T (hb_unicode_props_flags_t);

//   static inline void
//   _hb_glyph_info_set_unicode_props (hb_glyph_info_t *info, Buffer *buffer)
//   {
//     space_fallback *unicode = buffer->unicode;
//     unsigned int u = info->codepoint;
//     unsigned int gen_cat = (unsigned int) unicode->general_category (u);
//     unsigned int props = gen_cat;

//     if (u >= 0x80u)
//     {
//       if (unlikely (unicode->is_default_ignorable (u)))
//       {
//         buffer->scratch_flags |= HB_BUFFER_SCRATCH_FLAG_HAS_DEFAULT_IGNORABLES;
//         props |=  UPROPS_MASK_IGNORABLE;
//         if (u == 0x200Cu) props |= UPROPS_MASK_Cf_ZWNJ;
//         else if (u == 0x200Du) props |= UPROPS_MASK_Cf_ZWJ;
//         /* Mongolian Free Variation Selectors need to be remembered
//          * because although we need to hide them like default-ignorables,
//          * they need to non-ignorable during shaping.  This is similar to
//          * what we do for joiners in Indic-like shapers, but since the
//          * FVSes are GC=Mn, we have use a separate bit to remember them.
//          * Fixes:
//          * https://github.com/harfbuzz/harfbuzz/issues/234 */
//         else if (unlikely (hb_in_ranges<hb_codepoint_t> (u, 0x180Bu, 0x180Du, 0x180Fu, 0x180Fu))) props |= UPROPS_MASK_HIDDEN;
//         /* TAG characters need similar treatment. Fixes:
//          * https://github.com/harfbuzz/harfbuzz/issues/463 */
//         else if (unlikely (hb_in_range<hb_codepoint_t> (u, 0xE0020u, 0xE007Fu))) props |= UPROPS_MASK_HIDDEN;
//         /* COMBINING GRAPHEME JOINER should not be skipped; at least some times.
//          * https://github.com/harfbuzz/harfbuzz/issues/554 */
//         else if (unlikely (u == 0x034Fu))
//         {
//       buffer->scratch_flags |= HB_BUFFER_SCRATCH_FLAG_HAS_CGJ;
//       props |= UPROPS_MASK_HIDDEN;
//         }
//       }

//       if (unlikely (HB_UNICODE_GENERAL_CATEGORY_IS_MARK (gen_cat)))
//       {
//         props |= UPROPS_MASK_CONTINUATION;
//         props |= unicode->modified_combining_class (u)<<8;
//       }
//     }

//     info->unicode_props() = props;
//   }

impl GlyphInfo {
    /// HB: _hb_glyph_info_set_general_category
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L260>
    #[doc(alias = "_hb_glyph_info_set_general_category")]
    #[inline]
    pub(crate) fn set_general_category(&mut self, gen_cat: GeneralCategory) {
        /* Clears top-byte. */
        let gen_cat = gen_cat.0 as u32;
        let n = (gen_cat as u16)
            | (self.unicode_props() & (0xFF & !UnicodeProps::GENERAL_CATEGORY.bits()));
        self.set_unicode_props(n);
    }

    /// HB: _hb_glyph_info_get_general_category
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L268>
    #[doc(alias = "_hb_glyph_info_get_general_category")]
    #[inline]
    pub(crate) fn general_category(&self) -> GeneralCategory {
        let n = self.unicode_props() & UnicodeProps::GENERAL_CATEGORY.bits();
        GeneralCategory(n as u8)
    }

    /// HB: _hb_glyph_info_is_unicode_mark
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L274>
    #[doc(alias = "_hb_glyph_info_is_unicode_mark")]
    #[inline]
    pub(crate) fn is_unicode_mark(&self) -> bool {
        self.general_category().is_mark()
    }

    /// HB: _hb_glyph_info_set_modified_combining_class
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L279>
    #[doc(alias = "_hb_glyph_info_set_modified_combining_class")]
    #[inline]
    pub(crate) fn set_modified_combining_class(&mut self, modified_class: u8) {
        if !self.is_unicode_mark() {
            return;
        }
        let n = ((modified_class as u16) << 8) | (self.unicode_props() & 0xFF);
        self.set_unicode_props(n);
    }

    /// HB: _hb_glyph_info_get_modified_combining_class
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L287>
    #[doc(alias = "_hb_glyph_info_get_modified_combining_class")]
    #[inline]
    pub(crate) fn modified_combining_class(&self) -> u8 {
        if self.is_unicode_mark() {
            (self.unicode_props() >> 8) as u8
        } else {
            0
        }
    }

    // TODO: use
    // #[inline]
    // pub fn info_cc(info: &hb_glyph_info_t) -> u8 {
    //     _hb_glyph_info_get_modified_combining_class(info)
    // }

    /// HB: _hb_glyph_info_is_unicode_space
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L294>
    #[doc(alias = "_hb_glyph_info_is_unicode_space")]
    #[inline]
    pub(crate) fn is_unicode_space(&self) -> bool {
        self.general_category() == GeneralCategory::SPACE_SEPARATOR
    }

    /// HB: _hb_glyph_info_set_unicode_space_fallback_type
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L300>
    #[doc(alias = "_hb_glyph_info_set_unicode_space_fallback_type")]
    #[inline]
    pub(crate) fn set_unicode_space_fallback_type(&mut self, s: space_fallback::SpaceCode) {
        if !self.is_unicode_space() {
            return;
        }
        let n = ((s as u16) << 8) | (self.unicode_props() & 0xFF);
        self.set_unicode_props(n);
    }

    /// HB: _hb_glyph_info_get_unicode_space_fallback_type
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L307>
    #[doc(alias = "_hb_glyph_info_get_unicode_space_fallback_type")]
    #[inline]
    pub(crate) fn unicode_space_fallback_type(&self) -> space_fallback::SpaceCode {
        if self.is_unicode_space() {
            (self.unicode_props() >> 8) as u8
        } else {
            space_fallback::NOT_SPACE
        }
    }

    /// HB: _hb_glyph_info_is_variation_selector
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L314>
    #[doc(alias = "_hb_glyph_info_is_variation_selector")]
    #[inline]
    pub(crate) fn is_variation_selector(&self) -> bool {
        let a = self.general_category() == GeneralCategory::FORMAT;
        let b = (self.unicode_props() & UnicodeProps::CF_VS.bits()) != 0;
        a && b
    }

    /// HB: _hb_glyph_info_set_variation_selector
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L321>
    #[doc(alias = "_hb_glyph_info_set_variation_selector")]
    #[inline]
    pub(crate) fn set_variation_selector(&mut self, customize: bool) {
        if customize {
            self.set_general_category(GeneralCategory::FORMAT);
            self.set_unicode_props(self.unicode_props() | UnicodeProps::CF_VS.bits());
        } else {
            // Reset to their original condition
            self.set_general_category(GeneralCategory::NON_SPACING_MARK);
        }
    }

    /// HB: _hb_glyph_info_is_default_ignorable
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L338>
    #[doc(alias = "_hb_glyph_info_is_default_ignorable")]
    #[inline]
    pub(crate) fn is_default_ignorable(&self) -> bool {
        let n = self.unicode_props() & UnicodeProps::IGNORABLE.bits();
        n != 0 && !self.substituted()
    }

    /// HB: _hb_glyph_info_set_default_ignorable
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L344>
    #[doc(alias = "_hb_glyph_info_set_default_ignorable")]
    #[inline]
    pub(crate) fn _set_default_ignorable(&mut self) {
        self.set_unicode_props(self.unicode_props() | UnicodeProps::IGNORABLE.bits());
    }

    /// HB: _hb_glyph_info_clear_default_ignorable
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L349>
    #[doc(alias = "_hb_glyph_info_clear_default_ignorable")]
    #[inline]
    pub(crate) fn clear_default_ignorable(&mut self) {
        let mut n = self.unicode_props();
        n &= !UnicodeProps::IGNORABLE.bits();
        self.set_unicode_props(n);
    }

    /// HB: _hb_glyph_info_is_hidden
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L354>
    #[doc(alias = "_hb_glyph_info_is_hidden")]
    #[inline]
    pub(crate) fn is_hidden(&self) -> bool {
        (self.unicode_props() & UnicodeProps::HIDDEN.bits()) != 0
    }

    //   static inline void
    //   _hb_glyph_info_unhide (hb_glyph_info_t *info)
    //   {
    //     info->unicode_props() &= ~ UPROPS_MASK_HIDDEN;
    //   }

    /// HB: _hb_glyph_info_set_continuation
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L365>
    #[doc(alias = "_hb_glyph_info_set_continuation")]
    #[inline]
    pub(crate) fn set_continuation(&mut self, scratch_flags: &mut ScratchFlags) {
        *scratch_flags |= HB_BUFFER_SCRATCH_FLAG_HAS_CONTINUATIONS;
        let mut n = self.unicode_props();
        n |= UnicodeProps::CONTINUATION.bits();
        self.set_unicode_props(n);
    }

    /// HB: _hb_glyph_info_clear_continuation
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L370>
    #[doc(alias = "_hb_glyph_info_clear_continuation")]
    #[inline]
    pub(crate) fn clear_continuation(&mut self) {
        let mut n = self.unicode_props();
        n &= !UnicodeProps::CONTINUATION.bits();
        self.set_unicode_props(n);
    }

    /// HB: _hb_glyph_info_is_continuation
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L375>
    #[doc(alias = "_hb_glyph_info_is_continuation")]
    #[inline]
    pub(crate) fn is_continuation(&self) -> bool {
        self.unicode_props() & UnicodeProps::CONTINUATION.bits() != 0
    }
}

pub(crate) fn grapheme_group(_: &GlyphInfo, b: &GlyphInfo) -> bool {
    b.is_continuation()
}

pub fn reverse_graphemes(buffer: &mut Buffer) {
    // MONOTONE_GRAPHEMES was already applied and is taken care of by grapheme_group.
    // So we just check for MONOTONE_CHARACTERS here.
    buffer.reverse_groups(
        grapheme_group,
        buffer.cluster_level == HB_BUFFER_CLUSTER_LEVEL_MONOTONE_CHARACTERS,
    );
}

impl GlyphInfo {
    /// HB: _hb_glyph_info_is_unicode_format
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L398>
    #[doc(alias = "_hb_glyph_info_is_unicode_format")]
    #[inline]
    pub(crate) fn is_unicode_format(&self) -> bool {
        self.general_category() == GeneralCategory::FORMAT
    }

    /// HB: _hb_glyph_info_is_zwnj
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L404>
    #[doc(alias = "_hb_glyph_info_is_zwnj")]
    #[inline]
    pub(crate) fn is_zwnj(&self) -> bool {
        self.is_unicode_format() && (self.unicode_props() & UnicodeProps::CF_ZWNJ.bits() != 0)
    }

    /// HB: _hb_glyph_info_is_zwj
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L409>
    #[doc(alias = "_hb_glyph_info_is_zwnj")]
    #[inline]
    pub(crate) fn is_zwj(&self) -> bool {
        self.is_unicode_format() && (self.unicode_props() & UnicodeProps::CF_ZWJ.bits() != 0)
    }

    /// HB: _hb_glyph_info_is_aat_deleted
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L426>
    #[doc(alias = "_hb_glyph_info_is_aat_deleted")]
    #[inline]
    pub(crate) fn is_aat_deleted(&self) -> bool {
        self.is_unicode_format()
            && (self.unicode_props() & UnicodeProps::CF_AAT_DELETED.bits() != 0)
    }

    /// HB: _hb_glyph_info_set_aat_deleted
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L431>
    #[doc(alias = "_hb_glyph_info_set_aat_deleted")]
    #[inline]
    pub(crate) fn set_aat_deleted(&mut self) {
        self.set_general_category(GeneralCategory::FORMAT);
        self.set_unicode_props(
            self.unicode_props()
                | UnicodeProps::CF_AAT_DELETED.bits()
                | UnicodeProps::HIDDEN.bits(),
        );
    }

    //   /* lig_props: aka lig_id / lig_comp
    //    *
    //    * When a ligature is formed:
    //    *
    //    *   - The ligature glyph and any marks in between all the same newly allocated
    //    *     lig_id,
    //    *   - The ligature glyph will get lig_num_comps set to the number of components
    //    *   - The marks get lig_comp > 0, reflecting which component of the ligature
    //    *     they were applied to.
    //    *   - This is used in GPOS to attach marks to the right component of a ligature
    //    *     in MarkLigPos,
    //    *   - Note that when marks are ligated together, much of the above is skipped
    //    *     and the current lig_id reused.
    //    *
    //    * When a multiple-substitution is done:
    //    *
    //    *   - All resulting glyphs will have lig_id = 0,
    //    *   - The resulting glyphs will have lig_comp = 0, 1, 2, ... respectively.
    //    *   - This is used in GPOS to attach marks to the first component of a
    //    *     multiple substitution in MarkBasePos.
    //    *
    //    * The numbers are also used in GPOS to do mark-to-mark positioning only
    //    * to marks that belong to the same component of the same ligature.
    //    */
    //   static inline void
    //   _hb_glyph_info_clear_lig_props (hb_glyph_info_t *info)
    //   {
    //     info->lig_props() = 0;
    //   }

    const IS_LIG_BASE: u8 = 0x10;

    /// HB: _hb_glyph_info_set_lig_props_for_ligature
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L472>
    #[doc(alias = "_hb_glyph_info_set_lig_props_for_ligature")]
    #[inline]
    pub(crate) fn set_lig_props_for_ligature(&mut self, lig_id: u8, lig_num_comps: u8) {
        self.set_lig_props((lig_id << 5) | Self::IS_LIG_BASE | (lig_num_comps & 0x0F));
    }

    /// HB: _hb_glyph_info_set_lig_props_for_mark
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L480>
    #[doc(alias = "_hb_glyph_info_set_lig_props_for_mark")]
    #[inline]
    pub(crate) fn set_lig_props_for_mark(&mut self, lig_id: u8, lig_comp: u8) {
        self.set_lig_props((lig_id << 5) | (lig_comp & 0x0F));
    }

    /// HB: _hb_glyph_info_set_lig_props_for_component
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L488>
    #[doc(alias = "_hb_glyph_info_set_lig_props_for_component")]
    #[inline]
    pub(crate) fn set_lig_props_for_component(&mut self, comp: u8) {
        self.set_lig_props_for_mark(0, comp);
    }

    /// HB: _hb_glyph_info_get_lig_id
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L494>
    #[doc(alias = "_hb_glyph_info_get_lig_id")]
    #[inline]
    pub(crate) fn lig_id(&self) -> u8 {
        self.lig_props() >> 5
    }

    /// HB: _hb_glyph_info_ligated_internal
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L500>
    #[doc(alias = "_hb_glyph_info_ligated_internal")]
    #[inline]
    pub(crate) fn ligated_internal(&self) -> bool {
        self.lig_props() & Self::IS_LIG_BASE != 0
    }

    /// HB: _hb_glyph_info_get_lig_comp
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L506>
    #[doc(alias = "_hb_glyph_info_get_lig_comp")]
    #[inline]
    pub(crate) fn lig_comp(&self) -> u8 {
        if self.ligated_internal() {
            0
        } else {
            self.lig_props() & 0x0F
        }
    }

    /// HB: _hb_glyph_info_get_lig_num_comps
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L515>
    #[doc(alias = "_hb_glyph_info_get_lig_num_comps")]
    #[inline]
    pub(crate) fn lig_num_comps(&self) -> u8 {
        if self.glyph_props() & GlyphPropsFlags::LIGATURE.bits() != 0 && self.ligated_internal() {
            self.lig_props() & 0x0F
        } else {
            1
        }
    }

    //   /* glyph_props: */
    //   static inline void
    //   _hb_glyph_info_set_glyph_props (hb_glyph_info_t *info, unsigned int props)
    //   {
    //     info->glyph_props() = props;
    //   }

    //   static inline unsigned int
    //   _hb_glyph_info_get_glyph_props (const hb_glyph_info_t *info)
    //   {
    //     return info->glyph_props();
    //   }

    /// HB: _hb_glyph_info_is_base_glyph
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L548>
    #[doc(alias = "_hb_glyph_info_is_base_glyph")]
    #[inline]
    pub(crate) fn is_base_glyph(&self) -> bool {
        self.glyph_props() & GlyphPropsFlags::BASE_GLYPH.bits() != 0
    }

    /// HB: _hb_glyph_info_is_ligature
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L554>
    #[doc(alias = "_hb_glyph_info_is_ligature")]
    #[inline]
    pub(crate) fn is_ligature(&self) -> bool {
        self.glyph_props() & GlyphPropsFlags::LIGATURE.bits() != 0
    }

    /// HB: _hb_glyph_info_is_mark
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L560>
    #[doc(alias = "_hb_glyph_info_is_mark")]
    #[inline]
    pub(crate) fn is_mark(&self) -> bool {
        self.glyph_props() & GlyphPropsFlags::MARK.bits() != 0
    }

    /// HB: _hb_glyph_info_substituted
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L566>
    #[doc(alias = "_hb_glyph_info_substituted")]
    #[inline]
    pub(crate) fn substituted(&self) -> bool {
        self.glyph_props() & GlyphPropsFlags::SUBSTITUTED.bits() != 0
    }

    /// HB: _hb_glyph_info_ligated
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L572>
    #[doc(alias = "_hb_glyph_info_ligated")]
    #[inline]
    pub(crate) fn ligated(&self) -> bool {
        self.glyph_props() & GlyphPropsFlags::LIGATED.bits() != 0
    }

    /// HB: _hb_glyph_info_multiplied
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L578>
    #[doc(alias = "_hb_glyph_info_multiplied")]
    #[inline]
    pub(crate) fn multiplied(&self) -> bool {
        self.glyph_props() & GlyphPropsFlags::MULTIPLIED.bits() != 0
    }

    /// Returns the number of components this glyph contributes when forming a ligature.
    #[inline]
    pub(crate) fn lig_num_comps_in_ligation(&self) -> u8 {
        // When a glyph is decomposed by a MultipleSubst and its pieces later become
        // components of a ligature, the pieces belong to the same ligature component
        // as the first piece, matching how MarkBasePos attaches marks only to the
        // first piece. So the non-first pieces contribute no extra component.
        // https://github.com/harfbuzz/harfbuzz/issues/4969
        if self.multiplied() && self.lig_comp() != 0 {
            0
        } else {
            self.lig_num_comps()
        }
    }

    /// HB: _hb_glyph_info_ligated_and_didnt_multiply
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L584>
    #[doc(alias = "_hb_glyph_info_ligated_and_didnt_multiply")]
    #[inline]
    pub(crate) fn ligated_and_didnt_multiply(&self) -> bool {
        self.ligated() && !self.multiplied()
    }

    /// HB: _hb_glyph_info_clear_ligated_and_multiplied
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L590>
    #[doc(alias = "_hb_glyph_info_clear_ligated_and_multiplied")]
    #[inline]
    pub(crate) fn clear_ligated_and_multiplied(&mut self) {
        let mut n = self.glyph_props();
        n &= !(GlyphPropsFlags::LIGATED | GlyphPropsFlags::MULTIPLIED).bits();
        self.set_glyph_props(n);
    }

    /// HB: _hb_glyph_info_clear_substituted
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/368598b5bd9c37a15cb0fd5438b8e617e254609b/src/hb-ot-layout.hh#L597>
    #[doc(alias = "_hb_glyph_info_clear_substituted")]
    #[inline]
    pub(crate) fn clear_substituted(&mut self) {
        let mut n = self.glyph_props();
        n &= !GlyphPropsFlags::SUBSTITUTED.bits();
        self.set_glyph_props(n);
    }
}

pub fn clear_substitution_flags(
    _: &ShapePlan,
    _: &ShaperFont<'_, '_>,
    buffer: &mut Buffer,
) -> Option<bool> {
    for info in buffer.info_slice_mut() {
        info.clear_substituted();
    }

    Some(false)
}

#[cfg(test)]
mod candidate_tests {
    use super::*;

    #[test]
    fn dispatch_keeps_wide_keys_and_both_mask_halves_under_collisions() {
        struct EmptyTable;
        impl LayoutTable for EmptyTable {
            const KIND: LayoutTableKind = LayoutTableKind::Gpos;
            const IN_PLACE: bool = true;
            fn get_lookup(&self, _: u16) -> Option<&CachedLookup<'_>> {
                None
            }
            fn tables(&self) -> Option<&Arc<Tables>> {
                None
            }
        }
        let cache = DispatchRows::new(64);
        let glyph = 0x1_2345u32;
        let base = (glyph.wrapping_mul(0x9E37_79B9) >> 24) as usize;
        let neighbor = (base + 1) & 255;
        let tag = u64::from(glyph) << 32;
        cache.rows[base].store(u64::from(glyph + 1) << 32, Ordering::Relaxed);
        cache.rows[neighbor].store(tag | 3, Ordering::Relaxed);
        cache.high.as_ref().unwrap()[neighbor].store(tag | 0x8000_0001, Ordering::Relaxed);
        assert_eq!(
            cache.candidates(glyph, &[], &EmptyTable),
            0x8000_0001_0000_0003
        );
        assert_eq!(cache.candidates(glyph ^ 0x100_0000, &[], &EmptyTable), 0);
        assert_eq!(cache.candidates(u32::MAX, &[], &EmptyTable), 0);
    }

    #[test]
    fn pending_edits_preserve_unmodified_candidate_positions() {
        let mut glyphs: Vec<u32> = (0..64).collect();
        let infos: Vec<_> = glyphs
            .iter()
            .map(|&glyph_id| GlyphInfo {
                glyph_id,
                ..Default::default()
            })
            .collect();
        let mut columns = [NO_COLUMN; MAX_INDEXED_LOOKUPS];
        for (i, column) in columns[..5].iter_mut().enumerate() {
            *column = i as u16;
        }
        let mut index = CandidateIndex::new();
        index.words = vec![0; 5];
        index.begin(&infos, columns);
        let rebuild = |index: &mut CandidateIndex, glyphs: &[u32]| {
            index.words.fill(0);
            for (at, &glyph) in glyphs.iter().enumerate() {
                index.words[(glyph % 5) as usize] |= 1u64 << at;
                index.glyphs[at] = glyph;
            }
            index.dirty = 0;
        };
        rebuild(&mut index, &glyphs);
        let mut seed = 0x391A_02D3u32;
        let mut random = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for step in 0..512 {
            let at = random() as usize % (glyphs.len() + 1);
            let removed = (random() as usize % 4).min(glyphs.len() - at);
            let inserted = (random() as usize % 4).min(64 - (glyphs.len() - removed));
            let new: Vec<_> = (0..inserted).map(|_| random() & 0xFF_FFFF).collect();
            index.splice(at, removed, inserted);
            glyphs.splice(at..at + removed, new);
            assert!(index.active);
            assert_eq!(index.len, glyphs.len());
            assert_eq!(index.dirty & !low_bits(glyphs.len()), 0);
            for slot in 0..5 {
                let expected = glyphs.iter().enumerate().fold(0u64, |bits, (i, &glyph)| {
                    bits | if glyph % 5 == slot { 1 << i } else { 0 }
                });
                assert_eq!(
                    index.words[slot as usize] & !index.dirty,
                    expected & !index.dirty,
                    "Step {step}"
                );
            }
            if step % 7 == 0 {
                rebuild(&mut index, &glyphs);
            }
        }
    }

    #[test]
    fn growth_beyond_one_word_disables_the_index() {
        let infos = vec![GlyphInfo::default(); 64];
        let mut index = CandidateIndex::new();
        index.words.push(u64::MAX);
        index.begin(&infos, [0; MAX_INDEXED_LOOKUPS]);
        index.splice(64, 0, 1);
        assert!(!index.active);
        assert_eq!(index.words, [u64::MAX]);
    }
}
