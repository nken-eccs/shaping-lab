pub mod common {
    use super::layout::DELETED_GLYPH;
    use super::map::RangeFlags;
    use crate::buffer::{Buffer, HB_BUFFER_SCRATCH_FLAG_SHAPER0};
    use crate::ot::apply::MappingCache;
    use crate::plan::ShapePlan;
    use crate::set_digest::SetDigest;
    use crate::LayoutData;
    use crate::Mask;
    use crate::Scale;
    use crate::U32Set;
    use alloc::vec::Vec;
    use read_fonts::tables::aat::*;
    use read_fonts::types::{FixedSize, GlyphId};
    use read_fonts::FontData;

    pub const HB_BUFFER_SCRATCH_FLAG_AAT_HAS_DELETED: u32 = HB_BUFFER_SCRATCH_FLAG_SHAPER0;

    pub(crate) const START_OF_TEXT: u16 = 0;

    pub(crate) type ClassCache = MappingCache;

    pub(crate) fn get_class<T: bytemuck::AnyBitPattern + FixedSize>(
        machine: &ExtendedStateTable<'_, T>,
        glyph_id: GlyphId,
        cache: &ClassCache,
    ) -> u16 {
        if let Some(klass) = cache.get(glyph_id.to_u32()) {
            return klass as u16;
        }
        let klass = machine
            .class(glyph_id)
            .unwrap_or(class::OUT_OF_BOUNDS as u16);
        cache.set(glyph_id.to_u32(), klass as u32);
        klass
    }

    /// HB: hb_aat_apply_context_t
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/2c22a65f0cb99544c36580b9703a43b5dc97a9e1/src/hb-aat-layout-common.hh#L108>
    #[doc(alias = "hb_aat_apply_context_t")]
    pub struct AatApplyContext<'a, 'f, 'c> {
        pub plan: &'a ShapePlan,
        pub layout: LayoutData<'a>,
        pub scale: Scale,
        #[cfg(feature = "trace")]
        pub font: &'a crate::ShaperFont<'f, 'c>,
        #[cfg(not(feature = "trace"))]
        font_lifetimes: core::marker::PhantomData<(&'f (), &'c ())>,
        pub buffer: &'a mut Buffer,
        pub has_glyph_classes: bool,
        pub range_flags: Option<&'a [RangeFlags]>,
        pub subtable_flags: Mask,
        pub(crate) buffer_is_reversed: bool,
        // Caches
        using_buffer_glyph_set: bool,
        /// Whether the buffer's digest is current, to rule out subtables cheaply.
        using_buffer_digest: bool,
        pub(crate) first_set: Option<&'a U32Set>,
        /// A digest of `first_set`.
        pub(crate) first_digest: SetDigest,
        pub(crate) second_set: Option<&'a U32Set>,
        pub(crate) machine_class_cache: Option<&'a ClassCache>,
        pub(crate) start_end_safe_to_break: u64,
        pub(crate) safe_to_break: SafeToBreak<'a>,
    }

    // The font's lifetimes are named for the font that tracing takes.
    #[cfg_attr(not(feature = "trace"), allow(clippy::elidable_lifetime_names))]
    impl<'a, 'f, 'c> AatApplyContext<'a, 'f, 'c> {
        pub fn new(
            plan: &'a ShapePlan,
            layout: LayoutData<'a>,
            scale: Scale,
            buffer: &'a mut Buffer,
            #[cfg(feature = "trace")] font: &'a crate::ShaperFont<'f, 'c>,
        ) -> Self {
            Self {
                plan,
                layout,
                scale,
                #[cfg(not(feature = "trace"))]
                font_lifetimes: core::marker::PhantomData,
                buffer,
                #[cfg(feature = "trace")]
                font,
                has_glyph_classes: layout.ot.has_glyph_classes(),
                range_flags: None,
                subtable_flags: 0,
                buffer_is_reversed: false,
                using_buffer_glyph_set: false,
                using_buffer_digest: false,
                first_set: None,
                first_digest: SetDigest::full(),
                second_set: None,
                machine_class_cache: None,
                start_end_safe_to_break: 0,
                safe_to_break: SafeToBreak::default(),
            }
        }

        #[inline(always)]
        pub(crate) fn scale_x(&self, value: i32) -> i32 {
            self.scale.scale_x(value)
        }

        #[inline(always)]
        pub(crate) fn scale_y(&self, value: i32) -> i32 {
            self.scale.scale_y(value)
        }

        pub(crate) fn reverse_buffer(&mut self) {
            self.buffer.reverse();
            self.buffer_is_reversed = !self.buffer_is_reversed;
        }

        pub(crate) fn setup_buffer_glyph_set(&mut self) {
            self.using_buffer_glyph_set = self.buffer.len >= 4;

            if self.using_buffer_glyph_set {
                self.buffer.update_glyph_set();
            }
        }

        /// Prepares the digest that rules out subtables cheaply, if a table has
        /// enough subtables to repay building it.
        pub(crate) fn setup_buffer_digest(&mut self, subtable_count: usize) {
            self.using_buffer_digest = subtable_count >= MIN_DIGESTED_SUBTABLES;
            if self.using_buffer_digest {
                self.buffer.update_digest();
            }
        }

        pub(crate) fn buffer_intersects_machine(&self) -> bool {
            if let Some(first_set) = &self.first_set {
                if self.using_buffer_digest && !self.buffer.digest.may_intersect(&self.first_digest)
                {
                    return false;
                }
                if self.using_buffer_glyph_set {
                    return self.buffer.glyph_set.intersects_set(first_set);
                }
                for info in &self.buffer.info {
                    if first_set.contains(info.glyph_id) {
                        return true;
                    }
                }
                false
            } else {
                true
            }
        }

        pub fn output_glyph(&mut self, glyph: u32) -> Option<()> {
            self.buffer.digest.add(glyph);
            if self.using_buffer_glyph_set {
                self.buffer.glyph_set.insert(glyph);
            }

            // Insertion state machines can emit glyphs during the end-of-text
            // transition, when there is no current input glyph. output_glyph()
            // supports that by copying the previous output glyph's metadata.
            // Apply the AAT metadata to that new output glyph instead of indexing
            // past the end of the input buffer.
            let at_end = self.buffer.idx == self.buffer.len;
            if at_end {
                let out_len = self.buffer.out_len;
                self.buffer.output_glyph(glyph);
                if self.buffer.out_len == out_len {
                    return Some(());
                }
            }

            if glyph == DELETED_GLYPH {
                self.buffer.scratch_flags |= HB_BUFFER_SCRATCH_FLAG_AAT_HAS_DELETED;
                let info = if at_end {
                    self.buffer.prev_mut()?
                } else {
                    self.buffer.cur_mut(0)?
                };
                info.set_aat_deleted();
            } else {
                if self.has_glyph_classes {
                    let glyph_props = self.layout.ot.glyph_props(glyph.into());
                    let info = if at_end {
                        self.buffer.prev_mut()?
                    } else {
                        self.buffer.cur_mut(0)?
                    };
                    info.set_glyph_props(glyph_props);
                }
            }
            if !at_end {
                self.buffer.output_glyph(glyph);
            }
            Some(())
        }

        pub fn replace_glyph(&mut self, glyph: u32) -> Option<()> {
            if glyph == DELETED_GLYPH {
                self.buffer.scratch_flags |= HB_BUFFER_SCRATCH_FLAG_AAT_HAS_DELETED;
                self.buffer.cur_mut(0)?.set_aat_deleted();
            }

            self.buffer.digest.add(glyph);
            if self.using_buffer_glyph_set {
                self.buffer.glyph_set.insert(glyph);
            }
            if self.has_glyph_classes {
                self.buffer
                    .cur_mut(0)?
                    .set_glyph_props(self.layout.ot.glyph_props(glyph.into()));
            }
            self.buffer.replace_glyph(glyph);
            Some(())
        }

        pub fn delete_glyph(&mut self) -> Option<()> {
            self.buffer.scratch_flags |= HB_BUFFER_SCRATCH_FLAG_AAT_HAS_DELETED;
            self.buffer.cur_mut(0)?.set_aat_deleted();
            self.buffer.digest.add(DELETED_GLYPH);
            if self.using_buffer_glyph_set {
                self.buffer.glyph_set.insert(DELETED_GLYPH);
            }
            self.buffer.replace_glyph(DELETED_GLYPH);
            Some(())
        }

        #[inline(always)]
        pub fn replace_glyph_inplace(&mut self, i: usize, glyph: u32) -> Option<()> {
            let info = self.buffer.info.get_mut(i)?;
            info.glyph_id = glyph;
            if glyph == DELETED_GLYPH {
                self.buffer.scratch_flags |= HB_BUFFER_SCRATCH_FLAG_AAT_HAS_DELETED;
                info.set_aat_deleted();
            }
            self.buffer.digest.add(glyph);
            if self.using_buffer_glyph_set {
                self.buffer.glyph_set.insert(glyph);
            }
            if self.has_glyph_classes {
                info.set_glyph_props(self.layout.ot.glyph_props(glyph.into()));
            }
            Some(())
        }
    }

    /// Tables with fewer subtables than this skip the buffer digest.
    const MIN_DIGESTED_SUBTABLES: usize = 4;

    /// A digest of the glyphs in `set`.
    pub(crate) fn set_digest(set: &U32Set) -> SetDigest {
        let mut digest = SetDigest::new();
        for range in set.iter_ranges() {
            digest.add_range(*range.start(), *range.end());
        }
        digest
    }

    /// Per-table acceleration for the drive loops' safe-to-break computation.
    ///
    /// The data for every state-machine subtable in one AAT table is stored in
    /// two packed vectors. Each top-level subtable cache carries a compact
    /// descriptor for its ranges. This avoids two `Vec` allocations in every
    /// subtable cache while retaining O(classes) + O(states) storage.
    #[derive(Default)]
    pub(crate) struct SafeToBreakAccel {
        wouldbe: Vec<u16>,
        eot_tail: Vec<u64>,
    }

    #[derive(Clone, Copy, Default)]
    pub(crate) struct SafeToBreakSubtable {
        wouldbe_start: u32,
        wouldbe_end: u32,
        eot_tail_start: u32,
        eot_tail_end: u32,
    }

    /// The slice of [`SafeToBreakAccel`] belonging to the active subtable.
    #[derive(Clone, Copy, Default)]
    pub(crate) struct SafeToBreak<'a> {
        n_classes: u32,
        /// Per class: `entry(START_OF_TEXT, class)` packed as 15 bits of
        /// new state with the advance bit on top when present and
        /// non-actionable, `!0` otherwise — so condition 2c is a single
        /// compare. Real machines' state counts fit 15 bits.
        wouldbe: &'a [u16],
        /// Condition-3 bits for states 64 and up; states below 64 keep
        /// using the `start_end_safe_to_break` word. Empty for machines
        /// with at most 64 states.
        eot_tail: &'a [u64],
    }

    pub(crate) const WOULDBE_NONE: u16 = !0;

    #[inline(always)]
    pub(crate) fn pack_wouldbe(new_state: u16, advance: bool) -> u16 {
        // States fit 15 bits in any real font. A pathological new state
        // that doesn't can alias the sentinel and misreport condition 2c,
        // which only perturbs unsafe-to-break flags -- not worth runtime
        // checks on this path.
        new_state | ((advance as u16) << 15)
    }

    impl SafeToBreak<'_> {
        /// Condition 2c: would starting from the start state on this class
        /// take a non-actionable transition to the same state, advancing
        /// the same way?
        #[inline(always)]
        pub(crate) fn wouldbe_matches(&self, class: u16, next_state: u16, advance: bool) -> bool {
            let mut class = class as usize;
            if class >= self.n_classes as usize {
                class = class::OUT_OF_BOUNDS as usize;
            }
            self.wouldbe.get(class).copied() == Some(pack_wouldbe(next_state, advance))
        }

        /// Condition 3 for states 64 and up: no end-of-text action can fire
        /// out of this state.
        #[inline(always)]
        pub(crate) fn eot_safe_high(&self, state: u16) -> bool {
            let ix = (state - 64) as usize;
            self.eot_tail
                .get(ix / 64)
                .is_some_and(|word| word & (1 << (ix % 64)) != 0)
        }
    }

    impl SafeToBreakAccel {
        /// Returns a view of the acceleration data described by a top-level
        /// subtable cache.
        pub(crate) fn subtable(&self, subtable: SafeToBreakSubtable) -> Option<SafeToBreak<'_>> {
            let wouldbe_start = subtable.wouldbe_start as usize;
            let wouldbe_end = subtable.wouldbe_end as usize;
            let eot_tail_start = subtable.eot_tail_start as usize;
            let eot_tail_end = subtable.eot_tail_end as usize;

            Some(SafeToBreak {
                n_classes: subtable.wouldbe_end - subtable.wouldbe_start,
                wouldbe: self.wouldbe.get(wouldbe_start..wouldbe_end)?,
                eot_tail: self.eot_tail.get(eot_tail_start..eot_tail_end)?,
            })
        }

        /// Describes a subtable that does not contain a state machine.
        pub(crate) fn empty_subtable(&self) -> SafeToBreakSubtable {
            SafeToBreakSubtable {
                wouldbe_start: self.wouldbe.len() as u32,
                wouldbe_end: self.wouldbe.len() as u32,
                eot_tail_start: self.eot_tail.len() as u32,
                eot_tail_end: self.eot_tail.len() as u32,
            }
        }

        /// Appends the data for an extended state table whose machine
        /// starts at the beginning of `data`. The predicates mirror the
        /// subtable kind's notion of actionable/advancing entries.
        #[inline(never)]
        pub(crate) fn build_extended<T: bytemuck::AnyBitPattern + FixedSize>(
            &mut self,
            machine: &ExtendedStateTable<T>,
            data: &[u8],
            is_actionable: &dyn Fn(&StateEntry<T>) -> bool,
            can_advance: &dyn Fn(&StateEntry<T>) -> bool,
        ) -> SafeToBreakSubtable {
            let n_classes = machine.n_classes;
            let wouldbe_start = self.wouldbe.len();
            let eot_tail_start = self.eot_tail.len();

            // Cover the OUT_OF_BOUNDS column the runtime clamp can select
            // even on degenerate machines; entry() applies the same clamp
            // internally, so the aliasing matches.
            // Classes are u16 values, so columns past 0x10000 are
            // unreachable and need no slots.
            let wouldbe_len = n_classes
                .max(class::OUT_OF_BOUNDS as usize + 1)
                .min(1 << 16);
            self.wouldbe.reserve(wouldbe_len);
            for class in 0..wouldbe_len {
                self.wouldbe
                    .push(match machine.entry(START_OF_TEXT, class as u16) {
                        Ok(entry) if !is_actionable(&entry) => {
                            pack_wouldbe(entry.new_state, can_advance(&entry))
                        }
                        _ => WOULDBE_NONE,
                    });
            }

            // The state array runs from its offset to the end of the
            // subtable, matching how the table reader slices it; states
            // beyond it fail entry() and stay unsafe, like the probes they
            // replace.
            if n_classes > 0 {
                if let Ok(parts) = StateTableParts::read(FontData::new(data)) {
                    let n_cells = data.len().saturating_sub(parts.state_array_offset as usize)
                        / u16::RAW_BYTE_LEN;
                    let n_rows = n_cells.div_ceil(n_classes).min(u16::MAX as usize + 1);
                    if n_rows > 64 {
                        let eot_tail_start = self.eot_tail.len();
                        self.eot_tail
                            .resize(eot_tail_start + (n_rows - 64).div_ceil(64), 0);
                        for state in 64..n_rows {
                            if let Ok(entry) =
                                machine.entry(state as u16, u16::from(class::END_OF_TEXT))
                            {
                                if !is_actionable(&entry) {
                                    let ix = state - 64;
                                    self.eot_tail[eot_tail_start + ix / 64] |= 1 << (ix % 64);
                                }
                            }
                        }
                    }
                }
            }

            SafeToBreakSubtable::new(
                wouldbe_len,
                wouldbe_start,
                self.wouldbe.len(),
                eot_tail_start,
                self.eot_tail.len(),
            )
        }

        /// The legacy (`kern` Format1) counterpart of [`Self::build_extended`]:
        /// one-byte state cells and u8 classes.
        #[inline(never)]
        pub(crate) fn build_legacy<T: bytemuck::AnyBitPattern + FixedSize>(
            &mut self,
            machine: &StateTable<T>,
            is_actionable: &dyn Fn(&StateEntry<T>) -> bool,
            can_advance: &dyn Fn(&StateEntry<T>) -> bool,
        ) -> SafeToBreakSubtable {
            let n_classes = machine.header.state_size() as usize;
            let wouldbe_start = self.wouldbe.len();
            let eot_tail_start = self.eot_tail.len();

            let wouldbe_len = n_classes.max(class::OUT_OF_BOUNDS as usize + 1).min(1 << 8);
            self.wouldbe.reserve(wouldbe_len);
            for class in 0..wouldbe_len {
                self.wouldbe
                    .push(match machine.entry(START_OF_TEXT, class as u8) {
                        Ok(entry) if !is_actionable(&entry) => {
                            pack_wouldbe(entry.new_state, can_advance(&entry))
                        }
                        _ => WOULDBE_NONE,
                    });
            }

            if n_classes > 0 {
                if let Ok(state_array) = machine.header.state_array() {
                    let n_cells = state_array.data().len();
                    let n_rows = n_cells.div_ceil(n_classes).min(u16::MAX as usize + 1);
                    if n_rows > 64 {
                        let eot_tail_start = self.eot_tail.len();
                        self.eot_tail
                            .resize(eot_tail_start + (n_rows - 64).div_ceil(64), 0);
                        for state in 64..n_rows {
                            if let Ok(entry) = machine.entry(state as u16, class::END_OF_TEXT) {
                                if !is_actionable(&entry) {
                                    let ix = state - 64;
                                    self.eot_tail[eot_tail_start + ix / 64] |= 1 << (ix % 64);
                                }
                            }
                        }
                    }
                }
            }

            SafeToBreakSubtable::new(
                wouldbe_len,
                wouldbe_start,
                self.wouldbe.len(),
                eot_tail_start,
                self.eot_tail.len(),
            )
        }
    }

    impl SafeToBreakSubtable {
        fn new(
            wouldbe_len: usize,
            wouldbe_start: usize,
            wouldbe_end: usize,
            eot_tail_start: usize,
            eot_tail_end: usize,
        ) -> Self {
            debug_assert_eq!(wouldbe_end - wouldbe_start, wouldbe_len);
            Self {
                wouldbe_start: wouldbe_start as u32,
                wouldbe_end: wouldbe_end as u32,
                eot_tail_start: eot_tail_start as u32,
                eot_tail_end: eot_tail_end as u32,
            }
        }
    }

    // Keep one closure type for the morx and kerx class-set filters so that
    // their lookup collectors share a monomorphization.
    pub(crate) fn class_set_filter(classes: &U32Set) -> impl Fn(u16) -> bool + '_ {
        |class| classes.contains(u32::from(class))
    }

    pub trait TypedCollectGlyphs<T: LookupValue> {
        /// Add all indices into `set`.
        fn collect_glyphs(&self, set: &mut U32Set, num_glyphs: u32) {
            self.collect_glyphs_filtered::<_>(set, num_glyphs, |_| true);
        }

        /// For each valid index, read the value of type `T`.
        /// If `filter(&value)` returns true, insert the index into `set`.
        fn collect_glyphs_filtered<F>(&self, _set: &mut U32Set, _num_glyphs: u32, _filter: F)
        where
            F: Fn(T) -> bool;
    }

    impl<T> TypedCollectGlyphs<T> for TypedLookup<'_, T>
    where
        T: LookupValue,
    {
        fn collect_glyphs(&self, set: &mut U32Set, num_glyphs: u32) {
            self.lookup.collect_glyphs::<T>(set, num_glyphs);
        }
        fn collect_glyphs_filtered<F>(&self, set: &mut U32Set, num_glyphs: u32, filter: F)
        where
            F: Fn(T) -> bool,
        {
            self.lookup
                .collect_glyphs_filtered::<T, F>(set, num_glyphs, filter);
        }
    }

    pub trait CollectGlyphs {
        /// Add all indices into `set`.
        fn collect_glyphs<T>(&self, set: &mut U32Set, num_glyphs: u32)
        where
            T: LookupValue,
        {
            self.collect_glyphs_filtered::<T, _>(set, num_glyphs, |_| true);
        }

        /// For each valid index, read the value of type `T`.
        /// If `filter(&value)` returns true, insert the index into `set`.
        fn collect_glyphs_filtered<T, F>(&self, _set: &mut U32Set, _num_glyphs: u32, _filter: F)
        where
            T: LookupValue,
            F: Fn(T) -> bool;
    }

    impl CollectGlyphs for Lookup<'_> {
        fn collect_glyphs<T>(&self, set: &mut U32Set, num_glyphs: u32)
        where
            T: LookupValue,
        {
            match self {
                Lookup::Format0(lookup) => lookup.collect_glyphs::<T>(set, num_glyphs),
                Lookup::Format2(lookup) => lookup.collect_glyphs::<T>(set, num_glyphs),
                Lookup::Format4(lookup) => lookup.collect_glyphs::<T>(set, num_glyphs),
                Lookup::Format6(lookup) => lookup.collect_glyphs::<T>(set, num_glyphs),
                Lookup::Format8(lookup) => lookup.collect_glyphs::<T>(set, num_glyphs),
                Lookup::Format10(lookup) => lookup.collect_glyphs::<T>(set, num_glyphs),
            }
        }
        // Keep the shared collector out of the state-machine cache constructors.
        #[inline(never)]
        fn collect_glyphs_filtered<T, F>(&self, set: &mut U32Set, num_glyphs: u32, filter: F)
        where
            T: LookupValue,
            F: Fn(T) -> bool,
        {
            match self {
                Lookup::Format0(lookup) => {
                    lookup.collect_glyphs_filtered::<T, F>(set, num_glyphs, filter);
                }
                Lookup::Format2(lookup) => {
                    lookup.collect_glyphs_filtered::<T, F>(set, num_glyphs, filter);
                }
                Lookup::Format4(lookup) => {
                    lookup.collect_glyphs_filtered::<T, F>(set, num_glyphs, filter);
                }
                Lookup::Format6(lookup) => {
                    lookup.collect_glyphs_filtered::<T, F>(set, num_glyphs, filter);
                }
                Lookup::Format8(lookup) => {
                    lookup.collect_glyphs_filtered::<T, F>(set, num_glyphs, filter);
                }
                Lookup::Format10(lookup) => {
                    lookup.collect_glyphs_filtered::<T, F>(set, num_glyphs, filter);
                }
            }
        }
    }

    impl CollectGlyphs for Lookup0<'_> {
        fn collect_glyphs<T>(&self, set: &mut U32Set, num_glyphs: u32)
        where
            T: LookupValue,
        {
            set.insert_range(0..=num_glyphs.saturating_sub(1));
        }
        fn collect_glyphs_filtered<T, F>(&self, set: &mut U32Set, num_glyphs: u32, filter: F)
        where
            T: LookupValue,
            F: Fn(T) -> bool,
        {
            if let Ok(values) = self.values::<T>() {
                for (i, value) in values.iter().take(num_glyphs as usize).enumerate() {
                    if filter(value.get()) {
                        set.insert(i as u32);
                    }
                }
            }
        }
    }
    impl CollectGlyphs for Lookup2<'_> {
        fn collect_glyphs_filtered<T, F>(&self, set: &mut U32Set, _num_glyphs: u32, filter: F)
        where
            T: LookupValue,
            F: Fn(T) -> bool,
        {
            if let Ok(segments) = self.segments::<T>() {
                for segment in segments {
                    let value = segment.value;
                    if filter(value.get()) {
                        if segment.first_glyph.get() as u32 == DELETED_GLYPH {
                            continue;
                        }
                        set.insert_range(
                            segment.first_glyph.get() as u32..=segment.last_glyph.get() as u32,
                        );
                    }
                }
            }
        }
    }
    impl CollectGlyphs for Lookup4<'_> {
        fn collect_glyphs<T>(&self, set: &mut U32Set, _num_glyphs: u32)
        where
            T: LookupValue,
        {
            for segment in self.segments() {
                if segment.first_glyph.get() as u32 == DELETED_GLYPH {
                    continue;
                }
                set.insert_range(
                    segment.first_glyph.get() as u32..=segment.last_glyph.get() as u32,
                );
            }
        }
        fn collect_glyphs_filtered<T, F>(&self, set: &mut U32Set, _num_glyphs: u32, filter: F)
        where
            T: LookupValue,
            F: Fn(T) -> bool,
        {
            for (segment_idx, segment) in self.segments().iter().enumerate() {
                if segment.first_glyph.get() as u32 == DELETED_GLYPH {
                    continue;
                }
                let segment_values = self.segment_values(segment_idx);
                if let Ok(segment_values) = segment_values {
                    for (i, value) in segment_values.iter().enumerate() {
                        if filter(value.get()) {
                            set.insert(segment.first_glyph.get() as u32 + i as u32);
                        }
                    }
                }
            }
        }
    }
    impl CollectGlyphs for Lookup6<'_> {
        fn collect_glyphs_filtered<T, F>(&self, set: &mut U32Set, _num_glyphs: u32, filter: F)
        where
            T: LookupValue,
            F: Fn(T) -> bool,
        {
            let entries = self.entries();
            if let Ok(entries) = entries {
                for entry in entries {
                    let value = entry.value;
                    if filter(value.get()) {
                        if entry.glyph.get() as u32 == DELETED_GLYPH {
                            continue;
                        }
                        set.insert(entry.glyph.get() as u32);
                    }
                }
            }
        }
    }
    impl CollectGlyphs for Lookup8<'_> {
        fn collect_glyphs<T>(&self, set: &mut U32Set, _num_glyphs: u32)
        where
            T: LookupValue,
        {
            let n_values = self.value_array().len();
            let first_glyph = self.first_glyph();
            if first_glyph as u32 == DELETED_GLYPH {
                return;
            }
            set.insert_range(
                first_glyph as u32..=first_glyph as u32 + n_values.saturating_sub(1) as u32,
            );
        }
        fn collect_glyphs_filtered<T, F>(&self, set: &mut U32Set, _num_glyphs: u32, filter: F)
        where
            T: LookupValue,
            F: Fn(T) -> bool,
        {
            let values = self.value_array();
            let first_glyph = self.first_glyph();
            if first_glyph as u32 == DELETED_GLYPH {
                return;
            }
            for (i, value) in values.iter().enumerate() {
                if filter(T::from_u16(value.get())) {
                    set.insert(first_glyph as u32 + i as u32);
                }
            }
        }
    }
    impl CollectGlyphs for Lookup10<'_> {
        fn collect_glyphs<T>(&self, set: &mut U32Set, _num_glyphs: u32)
        where
            T: LookupValue,
        {
            let n_values = self.glyph_count();
            let first_glyph = self.first_glyph();
            if first_glyph as u32 == DELETED_GLYPH {
                return;
            }
            set.insert_range(
                first_glyph as u32..=first_glyph as u32 + n_values.saturating_sub(1) as u32,
            );
        }
        fn collect_glyphs_filtered<T, F>(&self, set: &mut U32Set, _num_glyphs: u32, filter: F)
        where
            T: LookupValue,
            F: Fn(T) -> bool,
        {
            let first_glyph = self.first_glyph();
            if first_glyph as u32 == DELETED_GLYPH {
                return;
            }
            for i in 0..self.glyph_count() {
                let idx = first_glyph as u32 + i as u32;
                // TODO: Speed up by accessing the value array directly
                let value = self.value::<T>(idx as u16);
                if let Ok(value) = value {
                    if filter(value) {
                        set.insert(idx);
                    }
                }
            }
        }
    }
}
pub mod kern;
pub mod kerx;
pub mod layout {
    #![allow(dead_code)]

    use super::map;
    use super::{kerx, morx, trak};
    use crate::aat::common::{AatApplyContext, HB_BUFFER_SCRATCH_FLAG_AAT_HAS_DELETED};
    use crate::Feature;
    use crate::LayoutData;
    use crate::{buffer::Buffer, plan::ShapePlan, GlyphInfo, Scale, Tag};

    pub type FeatureType = u8;

    pub const FEATURE_TYPE_INVALID: u8 = 0xFF;
    pub const FEATURE_TYPE_ALL_TYPOGRAPHIC: u8 = 0;
    pub const FEATURE_TYPE_LIGATURES: u8 = 1;
    pub const FEATURE_TYPE_CURSIVE_CONNECTION: u8 = 2;
    pub const FEATURE_TYPE_LETTER_CASE: u8 = 3;
    pub const FEATURE_TYPE_VERTICAL_SUBSTITUTION: u8 = 4;
    pub const FEATURE_TYPE_LINGUISTIC_REARRANGEMENT: u8 = 5;
    pub const FEATURE_TYPE_NUMBER_SPACING: u8 = 6;
    pub const FEATURE_TYPE_SMART_SWASH_TYPE: u8 = 8;
    pub const FEATURE_TYPE_DIACRITICS_TYPE: u8 = 9;
    pub const FEATURE_TYPE_VERTICAL_POSITION: u8 = 10;
    pub const FEATURE_TYPE_FRACTIONS: u8 = 11;
    pub const FEATURE_TYPE_OVERLAPPING_CHARACTERS_TYPE: u8 = 13;
    pub const FEATURE_TYPE_TYPOGRAPHIC_EXTRAS: u8 = 14;
    pub const FEATURE_TYPE_MATHEMATICAL_EXTRAS: u8 = 15;
    pub const FEATURE_TYPE_ORNAMENT_SETS_TYPE: u8 = 16;
    pub const FEATURE_TYPE_CHARACTER_ALTERNATIVES: u8 = 17;
    pub const FEATURE_TYPE_DESIGN_COMPLEXITY_TYPE: u8 = 18;
    pub const FEATURE_TYPE_STYLE_OPTIONS: u8 = 19;
    pub const FEATURE_TYPE_CHARACTER_SHAPE: u8 = 20;
    pub const FEATURE_TYPE_NUMBER_CASE: u8 = 21;
    pub const FEATURE_TYPE_TEXT_SPACING: u8 = 22;
    pub const FEATURE_TYPE_TRANSLITERATION: u8 = 23;
    pub const FEATURE_TYPE_ANNOTATION_TYPE: u8 = 24;
    pub const FEATURE_TYPE_KANA_SPACING_TYPE: u8 = 25;
    pub const FEATURE_TYPE_IDEOGRAPHIC_SPACING_TYPE: u8 = 26;
    pub const FEATURE_TYPE_UNICODE_DECOMPOSITION_TYPE: u8 = 27;
    pub const FEATURE_TYPE_RUBY_KANA: u8 = 28;
    pub const FEATURE_TYPE_CJK_SYMBOL_ALTERNATIVES_TYPE: u8 = 29;
    pub const FEATURE_TYPE_IDEOGRAPHIC_ALTERNATIVES_TYPE: u8 = 30;
    pub const FEATURE_TYPE_CJK_VERTICAL_ROMAN_PLACEMENT_TYPE: u8 = 31;
    pub const FEATURE_TYPE_ITALIC_CJK_ROMAN: u8 = 32;
    pub const FEATURE_TYPE_CASE_SENSITIVE_LAYOUT: u8 = 33;
    pub const FEATURE_TYPE_ALTERNATE_KANA: u8 = 34;
    pub const FEATURE_TYPE_STYLISTIC_ALTERNATIVES: u8 = 35;
    pub const FEATURE_TYPE_CONTEXTUAL_ALTERNATIVES: u8 = 36;
    pub const FEATURE_TYPE_LOWER_CASE: u8 = 37;
    pub const FEATURE_TYPE_UPPER_CASE: u8 = 38;
    pub const FEATURE_TYPE_LANGUAGE_TAG_TYPE: u8 = 39;
    pub const FEATURE_TYPE_CJK_ROMAN_SPACING_TYPE: u8 = 103;

    pub type FeatureSelector = u8;

    pub const FEATURE_SELECTOR_INVALID: u8 = 0xFF;

    /* Selectors for #FEATURE_TYPE_ALL_TYPOGRAPHIC */
    pub const FEATURE_SELECTOR_ALL_TYPE_FEATURES_ON: u8 = 0;
    pub const FEATURE_SELECTOR_ALL_TYPE_FEATURES_OFF: u8 = 1;

    /* Selectors for #FEATURE_TYPE_LIGATURES */
    pub const FEATURE_SELECTOR_REQUIRED_LIGATURES_ON: u8 = 0;
    pub const FEATURE_SELECTOR_REQUIRED_LIGATURES_OFF: u8 = 1;
    pub const FEATURE_SELECTOR_COMMON_LIGATURES_ON: u8 = 2;
    pub const FEATURE_SELECTOR_COMMON_LIGATURES_OFF: u8 = 3;
    pub const FEATURE_SELECTOR_RARE_LIGATURES_ON: u8 = 4;
    pub const FEATURE_SELECTOR_RARE_LIGATURES_OFF: u8 = 5;
    pub const FEATURE_SELECTOR_LOGOS_ON: u8 = 6;
    pub const FEATURE_SELECTOR_LOGOS_OFF: u8 = 7;
    pub const FEATURE_SELECTOR_REBUS_PICTURES_ON: u8 = 8;
    pub const FEATURE_SELECTOR_REBUS_PICTURES_OFF: u8 = 9;
    pub const FEATURE_SELECTOR_DIPHTHONG_LIGATURES_ON: u8 = 10;
    pub const FEATURE_SELECTOR_DIPHTHONG_LIGATURES_OFF: u8 = 11;
    pub const FEATURE_SELECTOR_SQUARED_LIGATURES_ON: u8 = 12;
    pub const FEATURE_SELECTOR_SQUARED_LIGATURES_OFF: u8 = 13;
    pub const FEATURE_SELECTOR_ABBREV_SQUARED_LIGATURES_ON: u8 = 14;
    pub const FEATURE_SELECTOR_ABBREV_SQUARED_LIGATURES_OFF: u8 = 15;
    pub const FEATURE_SELECTOR_SYMBOL_LIGATURES_ON: u8 = 16;
    pub const FEATURE_SELECTOR_SYMBOL_LIGATURES_OFF: u8 = 17;
    pub const FEATURE_SELECTOR_CONTEXTUAL_LIGATURES_ON: u8 = 18;
    pub const FEATURE_SELECTOR_CONTEXTUAL_LIGATURES_OFF: u8 = 19;
    pub const FEATURE_SELECTOR_HISTORICAL_LIGATURES_ON: u8 = 20;
    pub const FEATURE_SELECTOR_HISTORICAL_LIGATURES_OFF: u8 = 21;

    /* Selectors for #FEATURE_TYPE_LIGATURES */
    pub const FEATURE_SELECTOR_UNCONNECTED: u8 = 0;
    pub const FEATURE_SELECTOR_PARTIALLY_CONNECTED: u8 = 1;
    pub const FEATURE_SELECTOR_CURSIVE: u8 = 2;

    /* Selectors for #FEATURE_TYPE_LETTER_CASE */
    pub const FEATURE_SELECTOR_UPPER_AND_LOWER_CASE: u8 = 0; /* deprecated */
    pub const FEATURE_SELECTOR_ALL_CAPS: u8 = 1; /* deprecated */
    pub const FEATURE_SELECTOR_ALL_LOWER_CASE: u8 = 2; /* deprecated */
    pub const FEATURE_SELECTOR_SMALL_CAPS: u8 = 3; /* deprecated */
    pub const FEATURE_SELECTOR_INITIAL_CAPS: u8 = 4; /* deprecated */
    pub const FEATURE_SELECTOR_INITIAL_CAPS_AND_SMALL_CAPS: u8 = 5; /* deprecated */

    /* Selectors for #FEATURE_TYPE_VERTICAL_SUBSTITUTION */
    pub const FEATURE_SELECTOR_SUBSTITUTE_VERTICAL_FORMS_ON: u8 = 0;
    pub const FEATURE_SELECTOR_SUBSTITUTE_VERTICAL_FORMS_OFF: u8 = 1;

    /* Selectors for #FEATURE_TYPE_LINGUISTIC_REARRANGEMENT */
    pub const FEATURE_SELECTOR_LINGUISTIC_REARRANGEMENT_ON: u8 = 0;
    pub const FEATURE_SELECTOR_LINGUISTIC_REARRANGEMENT_OFF: u8 = 1;

    /* Selectors for #FEATURE_TYPE_NUMBER_SPACING */
    pub const FEATURE_SELECTOR_MONOSPACED_NUMBERS: u8 = 0;
    pub const FEATURE_SELECTOR_PROPORTIONAL_NUMBERS: u8 = 1;
    pub const FEATURE_SELECTOR_THIRD_WIDTH_NUMBERS: u8 = 2;
    pub const FEATURE_SELECTOR_QUARTER_WIDTH_NUMBERS: u8 = 3;

    /* Selectors for #FEATURE_TYPE_SMART_SWASH_TYPE */
    pub const FEATURE_SELECTOR_WORD_INITIAL_SWASHES_ON: u8 = 0;
    pub const FEATURE_SELECTOR_WORD_INITIAL_SWASHES_OFF: u8 = 1;
    pub const FEATURE_SELECTOR_WORD_FINAL_SWASHES_ON: u8 = 2;
    pub const FEATURE_SELECTOR_WORD_FINAL_SWASHES_OFF: u8 = 3;
    pub const FEATURE_SELECTOR_LINE_INITIAL_SWASHES_ON: u8 = 4;
    pub const FEATURE_SELECTOR_LINE_INITIAL_SWASHES_OFF: u8 = 5;
    pub const FEATURE_SELECTOR_LINE_FINAL_SWASHES_ON: u8 = 6;
    pub const FEATURE_SELECTOR_LINE_FINAL_SWASHES_OFF: u8 = 7;
    pub const FEATURE_SELECTOR_NON_FINAL_SWASHES_ON: u8 = 8;
    pub const FEATURE_SELECTOR_NON_FINAL_SWASHES_OFF: u8 = 9;

    /* Selectors for #FEATURE_TYPE_DIACRITICS_TYPE */
    pub const FEATURE_SELECTOR_SHOW_DIACRITICS: u8 = 0;
    pub const FEATURE_SELECTOR_HIDE_DIACRITICS: u8 = 1;
    pub const FEATURE_SELECTOR_DECOMPOSE_DIACRITICS: u8 = 2;

    /* Selectors for #FEATURE_TYPE_VERTICAL_POSITION */
    pub const FEATURE_SELECTOR_NORMAL_POSITION: u8 = 0;
    pub const FEATURE_SELECTOR_SUPERIORS: u8 = 1;
    pub const FEATURE_SELECTOR_INFERIORS: u8 = 2;
    pub const FEATURE_SELECTOR_ORDINALS: u8 = 3;
    pub const FEATURE_SELECTOR_SCIENTIFIC_INFERIORS: u8 = 4;

    /* Selectors for #FEATURE_TYPE_FRACTIONS */
    pub const FEATURE_SELECTOR_NO_FRACTIONS: u8 = 0;
    pub const FEATURE_SELECTOR_VERTICAL_FRACTIONS: u8 = 1;
    pub const FEATURE_SELECTOR_DIAGONAL_FRACTIONS: u8 = 2;

    /* Selectors for #FEATURE_TYPE_OVERLAPPING_CHARACTERS_TYPE */
    pub const FEATURE_SELECTOR_PREVENT_OVERLAP_ON: u8 = 0;
    pub const FEATURE_SELECTOR_PREVENT_OVERLAP_OFF: u8 = 1;

    /* Selectors for #FEATURE_TYPE_TYPOGRAPHIC_EXTRAS */
    pub const FEATURE_SELECTOR_HYPHENS_TO_EM_DASH_ON: u8 = 0;
    pub const FEATURE_SELECTOR_HYPHENS_TO_EM_DASH_OFF: u8 = 1;
    pub const FEATURE_SELECTOR_HYPHEN_TO_EN_DASH_ON: u8 = 2;
    pub const FEATURE_SELECTOR_HYPHEN_TO_EN_DASH_OFF: u8 = 3;
    pub const FEATURE_SELECTOR_SLASHED_ZERO_ON: u8 = 4;
    pub const FEATURE_SELECTOR_SLASHED_ZERO_OFF: u8 = 5;
    pub const FEATURE_SELECTOR_FORM_INTERROBANG_ON: u8 = 6;
    pub const FEATURE_SELECTOR_FORM_INTERROBANG_OFF: u8 = 7;
    pub const FEATURE_SELECTOR_SMART_QUOTES_ON: u8 = 8;
    pub const FEATURE_SELECTOR_SMART_QUOTES_OFF: u8 = 9;
    pub const FEATURE_SELECTOR_PERIODS_TO_ELLIPSIS_ON: u8 = 10;
    pub const FEATURE_SELECTOR_PERIODS_TO_ELLIPSIS_OFF: u8 = 11;

    /* Selectors for #FEATURE_TYPE_MATHEMATICAL_EXTRAS */
    pub const FEATURE_SELECTOR_HYPHEN_TO_MINUS_ON: u8 = 0;
    pub const FEATURE_SELECTOR_HYPHEN_TO_MINUS_OFF: u8 = 1;
    pub const FEATURE_SELECTOR_ASTERISK_TO_MULTIPLY_ON: u8 = 2;
    pub const FEATURE_SELECTOR_ASTERISK_TO_MULTIPLY_OFF: u8 = 3;
    pub const FEATURE_SELECTOR_SLASH_TO_DIVIDE_ON: u8 = 4;
    pub const FEATURE_SELECTOR_SLASH_TO_DIVIDE_OFF: u8 = 5;
    pub const FEATURE_SELECTOR_INEQUALITY_LIGATURES_ON: u8 = 6;
    pub const FEATURE_SELECTOR_INEQUALITY_LIGATURES_OFF: u8 = 7;
    pub const FEATURE_SELECTOR_EXPONENTS_ON: u8 = 8;
    pub const FEATURE_SELECTOR_EXPONENTS_OFF: u8 = 9;
    pub const FEATURE_SELECTOR_MATHEMATICAL_GREEK_ON: u8 = 10;
    pub const FEATURE_SELECTOR_MATHEMATICAL_GREEK_OFF: u8 = 11;

    /* Selectors for #FEATURE_TYPE_ORNAMENT_SETS_TYPE */
    pub const FEATURE_SELECTOR_NO_ORNAMENTS: u8 = 0;
    pub const FEATURE_SELECTOR_DINGBATS: u8 = 1;
    pub const FEATURE_SELECTOR_PI_CHARACTERS: u8 = 2;
    pub const FEATURE_SELECTOR_FLEURONS: u8 = 3;
    pub const FEATURE_SELECTOR_DECORATIVE_BORDERS: u8 = 4;
    pub const FEATURE_SELECTOR_INTERNATIONAL_SYMBOLS: u8 = 5;
    pub const FEATURE_SELECTOR_MATH_SYMBOLS: u8 = 6;

    /* Selectors for #FEATURE_TYPE_CHARACTER_ALTERNATIVES */
    pub const FEATURE_SELECTOR_NO_ALTERNATES: u8 = 0;

    /* Selectors for #FEATURE_TYPE_DESIGN_COMPLEXITY_TYPE */
    pub const FEATURE_SELECTOR_DESIGN_LEVEL1: u8 = 0;
    pub const FEATURE_SELECTOR_DESIGN_LEVEL2: u8 = 1;
    pub const FEATURE_SELECTOR_DESIGN_LEVEL3: u8 = 2;
    pub const FEATURE_SELECTOR_DESIGN_LEVEL4: u8 = 3;
    pub const FEATURE_SELECTOR_DESIGN_LEVEL5: u8 = 4;

    /* Selectors for #FEATURE_TYPE_STYLE_OPTIONS */
    pub const FEATURE_SELECTOR_NO_STYLE_OPTIONS: u8 = 0;
    pub const FEATURE_SELECTOR_DISPLAY_TEXT: u8 = 1;
    pub const FEATURE_SELECTOR_ENGRAVED_TEXT: u8 = 2;
    pub const FEATURE_SELECTOR_ILLUMINATED_CAPS: u8 = 3;
    pub const FEATURE_SELECTOR_TITLING_CAPS: u8 = 4;
    pub const FEATURE_SELECTOR_TALL_CAPS: u8 = 5;

    /* Selectors for #FEATURE_TYPE_CHARACTER_SHAPE */
    pub const FEATURE_SELECTOR_TRADITIONAL_CHARACTERS: u8 = 0;
    pub const FEATURE_SELECTOR_SIMPLIFIED_CHARACTERS: u8 = 1;
    pub const FEATURE_SELECTOR_JIS1978_CHARACTERS: u8 = 2;
    pub const FEATURE_SELECTOR_JIS1983_CHARACTERS: u8 = 3;
    pub const FEATURE_SELECTOR_JIS1990_CHARACTERS: u8 = 4;
    pub const FEATURE_SELECTOR_TRADITIONAL_ALT_ONE: u8 = 5;
    pub const FEATURE_SELECTOR_TRADITIONAL_ALT_TWO: u8 = 6;
    pub const FEATURE_SELECTOR_TRADITIONAL_ALT_THREE: u8 = 7;
    pub const FEATURE_SELECTOR_TRADITIONAL_ALT_FOUR: u8 = 8;
    pub const FEATURE_SELECTOR_TRADITIONAL_ALT_FIVE: u8 = 9;
    pub const FEATURE_SELECTOR_EXPERT_CHARACTERS: u8 = 10;
    pub const FEATURE_SELECTOR_JIS2004_CHARACTERS: u8 = 11;
    pub const FEATURE_SELECTOR_HOJO_CHARACTERS: u8 = 12;
    pub const FEATURE_SELECTOR_NLCCHARACTERS: u8 = 13;
    pub const FEATURE_SELECTOR_TRADITIONAL_NAMES_CHARACTERS: u8 = 14;

    /* Selectors for #FEATURE_TYPE_NUMBER_CASE */
    pub const FEATURE_SELECTOR_LOWER_CASE_NUMBERS: u8 = 0;
    pub const FEATURE_SELECTOR_UPPER_CASE_NUMBERS: u8 = 1;

    /* Selectors for #FEATURE_TYPE_TEXT_SPACING */
    pub const FEATURE_SELECTOR_PROPORTIONAL_TEXT: u8 = 0;
    pub const FEATURE_SELECTOR_MONOSPACED_TEXT: u8 = 1;
    pub const FEATURE_SELECTOR_HALF_WIDTH_TEXT: u8 = 2;
    pub const FEATURE_SELECTOR_THIRD_WIDTH_TEXT: u8 = 3;
    pub const FEATURE_SELECTOR_QUARTER_WIDTH_TEXT: u8 = 4;
    pub const FEATURE_SELECTOR_ALT_PROPORTIONAL_TEXT: u8 = 5;
    pub const FEATURE_SELECTOR_ALT_HALF_WIDTH_TEXT: u8 = 6;

    /* Selectors for #FEATURE_TYPE_TRANSLITERATION */
    pub const FEATURE_SELECTOR_NO_TRANSLITERATION: u8 = 0;
    pub const FEATURE_SELECTOR_HANJA_TO_HANGUL: u8 = 1;
    pub const FEATURE_SELECTOR_HIRAGANA_TO_KATAKANA: u8 = 2;
    pub const FEATURE_SELECTOR_KATAKANA_TO_HIRAGANA: u8 = 3;
    pub const FEATURE_SELECTOR_KANA_TO_ROMANIZATION: u8 = 4;
    pub const FEATURE_SELECTOR_ROMANIZATION_TO_HIRAGANA: u8 = 5;
    pub const FEATURE_SELECTOR_ROMANIZATION_TO_KATAKANA: u8 = 6;
    pub const FEATURE_SELECTOR_HANJA_TO_HANGUL_ALT_ONE: u8 = 7;
    pub const FEATURE_SELECTOR_HANJA_TO_HANGUL_ALT_TWO: u8 = 8;
    pub const FEATURE_SELECTOR_HANJA_TO_HANGUL_ALT_THREE: u8 = 9;

    /* Selectors for #FEATURE_TYPE_ANNOTATION_TYPE */
    pub const FEATURE_SELECTOR_NO_ANNOTATION: u8 = 0;
    pub const FEATURE_SELECTOR_BOX_ANNOTATION: u8 = 1;
    pub const FEATURE_SELECTOR_ROUNDED_BOX_ANNOTATION: u8 = 2;
    pub const FEATURE_SELECTOR_CIRCLE_ANNOTATION: u8 = 3;
    pub const FEATURE_SELECTOR_INVERTED_CIRCLE_ANNOTATION: u8 = 4;
    pub const FEATURE_SELECTOR_PARENTHESIS_ANNOTATION: u8 = 5;
    pub const FEATURE_SELECTOR_PERIOD_ANNOTATION: u8 = 6;
    pub const FEATURE_SELECTOR_ROMAN_NUMERAL_ANNOTATION: u8 = 7;
    pub const FEATURE_SELECTOR_DIAMOND_ANNOTATION: u8 = 8;
    pub const FEATURE_SELECTOR_INVERTED_BOX_ANNOTATION: u8 = 9;
    pub const FEATURE_SELECTOR_INVERTED_ROUNDED_BOX_ANNOTATION: u8 = 10;

    /* Selectors for #FEATURE_TYPE_KANA_SPACING_TYPE */
    pub const FEATURE_SELECTOR_FULL_WIDTH_KANA: u8 = 0;
    pub const FEATURE_SELECTOR_PROPORTIONAL_KANA: u8 = 1;

    /* Selectors for #FEATURE_TYPE_IDEOGRAPHIC_SPACING_TYPE */
    pub const FEATURE_SELECTOR_FULL_WIDTH_IDEOGRAPHS: u8 = 0;
    pub const FEATURE_SELECTOR_PROPORTIONAL_IDEOGRAPHS: u8 = 1;
    pub const FEATURE_SELECTOR_HALF_WIDTH_IDEOGRAPHS: u8 = 2;

    /* Selectors for #FEATURE_TYPE_UNICODE_DECOMPOSITION_TYPE */
    pub const FEATURE_SELECTOR_CANONICAL_COMPOSITION_ON: u8 = 0;
    pub const FEATURE_SELECTOR_CANONICAL_COMPOSITION_OFF: u8 = 1;
    pub const FEATURE_SELECTOR_COMPATIBILITY_COMPOSITION_ON: u8 = 2;
    pub const FEATURE_SELECTOR_COMPATIBILITY_COMPOSITION_OFF: u8 = 3;
    pub const FEATURE_SELECTOR_TRANSCODING_COMPOSITION_ON: u8 = 4;
    pub const FEATURE_SELECTOR_TRANSCODING_COMPOSITION_OFF: u8 = 5;

    /* Selectors for #FEATURE_TYPE_RUBY_KANA */
    pub const FEATURE_SELECTOR_NO_RUBY_KANA: u8 = 0; /* deprecated - use FEATURE_SELECTOR_RUBY_KANA_OFF instead */
    pub const FEATURE_SELECTOR_RUBY_KANA: u8 = 1; /* deprecated - use FEATURE_SELECTOR_RUBY_KANA_ON instead */
    pub const FEATURE_SELECTOR_RUBY_KANA_ON: u8 = 2;
    pub const FEATURE_SELECTOR_RUBY_KANA_OFF: u8 = 3;

    /* Selectors for #FEATURE_TYPE_CJK_SYMBOL_ALTERNATIVES_TYPE */
    pub const FEATURE_SELECTOR_NO_CJK_SYMBOL_ALTERNATIVES: u8 = 0;
    pub const FEATURE_SELECTOR_CJK_SYMBOL_ALT_ONE: u8 = 1;
    pub const FEATURE_SELECTOR_CJK_SYMBOL_ALT_TWO: u8 = 2;
    pub const FEATURE_SELECTOR_CJK_SYMBOL_ALT_THREE: u8 = 3;
    pub const FEATURE_SELECTOR_CJK_SYMBOL_ALT_FOUR: u8 = 4;
    pub const FEATURE_SELECTOR_CJK_SYMBOL_ALT_FIVE: u8 = 5;

    /* Selectors for #FEATURE_TYPE_IDEOGRAPHIC_ALTERNATIVES_TYPE */
    pub const FEATURE_SELECTOR_NO_IDEOGRAPHIC_ALTERNATIVES: u8 = 0;
    pub const FEATURE_SELECTOR_IDEOGRAPHIC_ALT_ONE: u8 = 1;
    pub const FEATURE_SELECTOR_IDEOGRAPHIC_ALT_TWO: u8 = 2;
    pub const FEATURE_SELECTOR_IDEOGRAPHIC_ALT_THREE: u8 = 3;
    pub const FEATURE_SELECTOR_IDEOGRAPHIC_ALT_FOUR: u8 = 4;
    pub const FEATURE_SELECTOR_IDEOGRAPHIC_ALT_FIVE: u8 = 5;

    /* Selectors for #FEATURE_TYPE_CJK_VERTICAL_ROMAN_PLACEMENT_TYPE */
    pub const FEATURE_SELECTOR_CJK_VERTICAL_ROMAN_CENTERED: u8 = 0;
    pub const FEATURE_SELECTOR_CJK_VERTICAL_ROMAN_HBASELINE: u8 = 1;

    /* Selectors for #FEATURE_TYPE_ITALIC_CJK_ROMAN */
    pub const FEATURE_SELECTOR_NO_CJK_ITALIC_ROMAN: u8 = 0; /* deprecated - use FEATURE_SELECTOR_CJK_ITALIC_ROMAN_OFF instead */
    pub const FEATURE_SELECTOR_CJK_ITALIC_ROMAN: u8 = 1; /* deprecated - use FEATURE_SELECTOR_CJK_ITALIC_ROMAN_ON instead */
    pub const FEATURE_SELECTOR_CJK_ITALIC_ROMAN_ON: u8 = 2;
    pub const FEATURE_SELECTOR_CJK_ITALIC_ROMAN_OFF: u8 = 3;

    /* Selectors for #FEATURE_TYPE_CASE_SENSITIVE_LAYOUT */
    pub const FEATURE_SELECTOR_CASE_SENSITIVE_LAYOUT_ON: u8 = 0;
    pub const FEATURE_SELECTOR_CASE_SENSITIVE_LAYOUT_OFF: u8 = 1;
    pub const FEATURE_SELECTOR_CASE_SENSITIVE_SPACING_ON: u8 = 2;
    pub const FEATURE_SELECTOR_CASE_SENSITIVE_SPACING_OFF: u8 = 3;

    /* Selectors for #FEATURE_TYPE_ALTERNATE_KANA */
    pub const FEATURE_SELECTOR_ALTERNATE_HORIZ_KANA_ON: u8 = 0;
    pub const FEATURE_SELECTOR_ALTERNATE_HORIZ_KANA_OFF: u8 = 1;
    pub const FEATURE_SELECTOR_ALTERNATE_VERT_KANA_ON: u8 = 2;
    pub const FEATURE_SELECTOR_ALTERNATE_VERT_KANA_OFF: u8 = 3;

    /* Selectors for #FEATURE_TYPE_STYLISTIC_ALTERNATIVES */
    pub const FEATURE_SELECTOR_NO_STYLISTIC_ALTERNATES: u8 = 0;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_ONE_ON: u8 = 2;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_ONE_OFF: u8 = 3;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_TWO_ON: u8 = 4;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_TWO_OFF: u8 = 5;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_THREE_ON: u8 = 6;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_THREE_OFF: u8 = 7;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_FOUR_ON: u8 = 8;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_FOUR_OFF: u8 = 9;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_FIVE_ON: u8 = 10;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_FIVE_OFF: u8 = 11;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_SIX_ON: u8 = 12;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_SIX_OFF: u8 = 13;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_SEVEN_ON: u8 = 14;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_SEVEN_OFF: u8 = 15;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_EIGHT_ON: u8 = 16;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_EIGHT_OFF: u8 = 17;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_NINE_ON: u8 = 18;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_NINE_OFF: u8 = 19;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_TEN_ON: u8 = 20;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_TEN_OFF: u8 = 21;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_ELEVEN_ON: u8 = 22;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_ELEVEN_OFF: u8 = 23;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_TWELVE_ON: u8 = 24;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_TWELVE_OFF: u8 = 25;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_THIRTEEN_ON: u8 = 26;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_THIRTEEN_OFF: u8 = 27;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_FOURTEEN_ON: u8 = 28;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_FOURTEEN_OFF: u8 = 29;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_FIFTEEN_ON: u8 = 30;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_FIFTEEN_OFF: u8 = 31;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_SIXTEEN_ON: u8 = 32;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_SIXTEEN_OFF: u8 = 33;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_SEVENTEEN_ON: u8 = 34;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_SEVENTEEN_OFF: u8 = 35;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_EIGHTEEN_ON: u8 = 36;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_EIGHTEEN_OFF: u8 = 37;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_NINETEEN_ON: u8 = 38;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_NINETEEN_OFF: u8 = 39;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_TWENTY_ON: u8 = 40;
    pub const FEATURE_SELECTOR_STYLISTIC_ALT_TWENTY_OFF: u8 = 41;

    /* Selectors for #FEATURE_TYPE_CONTEXTUAL_ALTERNATIVES */
    pub const FEATURE_SELECTOR_CONTEXTUAL_ALTERNATES_ON: u8 = 0;
    pub const FEATURE_SELECTOR_CONTEXTUAL_ALTERNATES_OFF: u8 = 1;
    pub const FEATURE_SELECTOR_SWASH_ALTERNATES_ON: u8 = 2;
    pub const FEATURE_SELECTOR_SWASH_ALTERNATES_OFF: u8 = 3;
    pub const FEATURE_SELECTOR_CONTEXTUAL_SWASH_ALTERNATES_ON: u8 = 4;
    pub const FEATURE_SELECTOR_CONTEXTUAL_SWASH_ALTERNATES_OFF: u8 = 5;

    /* Selectors for #FEATURE_TYPE_LOWER_CASE */
    pub const FEATURE_SELECTOR_DEFAULT_LOWER_CASE: u8 = 0;
    pub const FEATURE_SELECTOR_LOWER_CASE_SMALL_CAPS: u8 = 1;
    pub const FEATURE_SELECTOR_LOWER_CASE_PETITE_CAPS: u8 = 2;

    /* Selectors for #FEATURE_TYPE_UPPER_CASE */
    pub const FEATURE_SELECTOR_DEFAULT_UPPER_CASE: u8 = 0;
    pub const FEATURE_SELECTOR_UPPER_CASE_SMALL_CAPS: u8 = 1;
    pub const FEATURE_SELECTOR_UPPER_CASE_PETITE_CAPS: u8 = 2;

    /* Selectors for #FEATURE_TYPE_CJK_ROMAN_SPACING_TYPE */
    pub const FEATURE_SELECTOR_HALF_WIDTH_CJK_ROMAN: u8 = 0;
    pub const FEATURE_SELECTOR_PROPORTIONAL_CJK_ROMAN: u8 = 1;
    pub const FEATURE_SELECTOR_DEFAULT_CJK_ROMAN: u8 = 2;
    pub const FEATURE_SELECTOR_FULL_WIDTH_CJK_ROMAN: u8 = 3;

    /// HB: hb_aat_feature_mapping_t
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/2c22a65f0cb99544c36580b9703a43b5dc97a9e1/src/hb-aat-layout.hh#L38>
    pub struct FeatureMapping {
        pub ot_feature_tag: Tag,
        pub aat_feature_type: FeatureType,
        pub selector_to_enable: u8,
        pub selector_to_disable: u8,
    }

    impl FeatureMapping {
        const fn new(
            ot_feature_tag: &[u8; 4],
            aat_feature_type: FeatureType,
            selector_to_enable: u8,
            selector_to_disable: u8,
        ) -> Self {
            FeatureMapping {
                ot_feature_tag: Tag::new(ot_feature_tag),
                aat_feature_type,
                selector_to_enable,
                selector_to_disable,
            }
        }
    }

    /// Mapping from OpenType feature tags to AAT feature names and selectors.
    ///
    /// Table data courtesy of Apple.
    /// Converted from mnemonics to integers when moving to this file.
    #[rustfmt::skip]
    pub static feature_mappings: &[FeatureMapping] = &[
        FeatureMapping::new(b"afrc", FEATURE_TYPE_FRACTIONS, FEATURE_SELECTOR_VERTICAL_FRACTIONS, FEATURE_SELECTOR_NO_FRACTIONS),
        FeatureMapping::new(b"c2pc", FEATURE_TYPE_UPPER_CASE, FEATURE_SELECTOR_UPPER_CASE_PETITE_CAPS, FEATURE_SELECTOR_DEFAULT_UPPER_CASE),
        FeatureMapping::new(b"c2sc", FEATURE_TYPE_UPPER_CASE, FEATURE_SELECTOR_UPPER_CASE_SMALL_CAPS, FEATURE_SELECTOR_DEFAULT_UPPER_CASE),
        FeatureMapping::new(b"calt", FEATURE_TYPE_CONTEXTUAL_ALTERNATIVES, FEATURE_SELECTOR_CONTEXTUAL_ALTERNATES_ON, FEATURE_SELECTOR_CONTEXTUAL_ALTERNATES_OFF),
        FeatureMapping::new(b"case", FEATURE_TYPE_CASE_SENSITIVE_LAYOUT, FEATURE_SELECTOR_CASE_SENSITIVE_LAYOUT_ON, FEATURE_SELECTOR_CASE_SENSITIVE_LAYOUT_OFF),
        FeatureMapping::new(b"clig", FEATURE_TYPE_LIGATURES, FEATURE_SELECTOR_CONTEXTUAL_LIGATURES_ON, FEATURE_SELECTOR_CONTEXTUAL_LIGATURES_OFF),
        FeatureMapping::new(b"cpsp", FEATURE_TYPE_CASE_SENSITIVE_LAYOUT, FEATURE_SELECTOR_CASE_SENSITIVE_SPACING_ON, FEATURE_SELECTOR_CASE_SENSITIVE_SPACING_OFF),
        FeatureMapping::new(b"cswh", FEATURE_TYPE_CONTEXTUAL_ALTERNATIVES, FEATURE_SELECTOR_CONTEXTUAL_SWASH_ALTERNATES_ON, FEATURE_SELECTOR_CONTEXTUAL_SWASH_ALTERNATES_OFF),
        FeatureMapping::new(b"dlig", FEATURE_TYPE_LIGATURES, FEATURE_SELECTOR_RARE_LIGATURES_ON, FEATURE_SELECTOR_RARE_LIGATURES_OFF),
        FeatureMapping::new(b"expt", FEATURE_TYPE_CHARACTER_SHAPE, FEATURE_SELECTOR_EXPERT_CHARACTERS, 16),
        FeatureMapping::new(b"frac", FEATURE_TYPE_FRACTIONS, FEATURE_SELECTOR_DIAGONAL_FRACTIONS, FEATURE_SELECTOR_NO_FRACTIONS),
        FeatureMapping::new(b"fwid", FEATURE_TYPE_TEXT_SPACING, FEATURE_SELECTOR_MONOSPACED_TEXT, 7),
        FeatureMapping::new(b"halt", FEATURE_TYPE_TEXT_SPACING, FEATURE_SELECTOR_ALT_HALF_WIDTH_TEXT, 7),
        FeatureMapping::new(b"hist", 40, 0, 1),
        FeatureMapping::new(b"hkna", FEATURE_TYPE_ALTERNATE_KANA, FEATURE_SELECTOR_ALTERNATE_HORIZ_KANA_ON, FEATURE_SELECTOR_ALTERNATE_HORIZ_KANA_OFF),
        FeatureMapping::new(b"hlig", FEATURE_TYPE_LIGATURES, FEATURE_SELECTOR_HISTORICAL_LIGATURES_ON, FEATURE_SELECTOR_HISTORICAL_LIGATURES_OFF),
        FeatureMapping::new(b"hngl", FEATURE_TYPE_TRANSLITERATION, FEATURE_SELECTOR_HANJA_TO_HANGUL, FEATURE_SELECTOR_NO_TRANSLITERATION),
        FeatureMapping::new(b"hojo", FEATURE_TYPE_CHARACTER_SHAPE, FEATURE_SELECTOR_HOJO_CHARACTERS, 16),
        FeatureMapping::new(b"hwid", FEATURE_TYPE_TEXT_SPACING, FEATURE_SELECTOR_HALF_WIDTH_TEXT, 7),
        FeatureMapping::new(b"ital", FEATURE_TYPE_ITALIC_CJK_ROMAN, FEATURE_SELECTOR_CJK_ITALIC_ROMAN_ON, FEATURE_SELECTOR_CJK_ITALIC_ROMAN_OFF),
        FeatureMapping::new(b"jp04", FEATURE_TYPE_CHARACTER_SHAPE, FEATURE_SELECTOR_JIS2004_CHARACTERS, 16),
        FeatureMapping::new(b"jp78", FEATURE_TYPE_CHARACTER_SHAPE, FEATURE_SELECTOR_JIS1978_CHARACTERS, 16),
        FeatureMapping::new(b"jp83", FEATURE_TYPE_CHARACTER_SHAPE, FEATURE_SELECTOR_JIS1983_CHARACTERS, 16),
        FeatureMapping::new(b"jp90", FEATURE_TYPE_CHARACTER_SHAPE, FEATURE_SELECTOR_JIS1990_CHARACTERS, 16),
        FeatureMapping::new(b"liga", FEATURE_TYPE_LIGATURES, FEATURE_SELECTOR_COMMON_LIGATURES_ON, FEATURE_SELECTOR_COMMON_LIGATURES_OFF),
        FeatureMapping::new(b"lnum", FEATURE_TYPE_NUMBER_CASE, FEATURE_SELECTOR_UPPER_CASE_NUMBERS, 2),
        FeatureMapping::new(b"mgrk", FEATURE_TYPE_MATHEMATICAL_EXTRAS, FEATURE_SELECTOR_MATHEMATICAL_GREEK_ON, FEATURE_SELECTOR_MATHEMATICAL_GREEK_OFF),
        FeatureMapping::new(b"nlck", FEATURE_TYPE_CHARACTER_SHAPE, FEATURE_SELECTOR_NLCCHARACTERS, 16),
        FeatureMapping::new(b"onum", FEATURE_TYPE_NUMBER_CASE, FEATURE_SELECTOR_LOWER_CASE_NUMBERS, 2),
        FeatureMapping::new(b"ordn", FEATURE_TYPE_VERTICAL_POSITION, FEATURE_SELECTOR_ORDINALS, FEATURE_SELECTOR_NORMAL_POSITION),
        FeatureMapping::new(b"palt", FEATURE_TYPE_TEXT_SPACING, FEATURE_SELECTOR_ALT_PROPORTIONAL_TEXT, 7),
        FeatureMapping::new(b"pcap", FEATURE_TYPE_LOWER_CASE, FEATURE_SELECTOR_LOWER_CASE_PETITE_CAPS, FEATURE_SELECTOR_DEFAULT_LOWER_CASE),
        FeatureMapping::new(b"pkna", FEATURE_TYPE_TEXT_SPACING, FEATURE_SELECTOR_PROPORTIONAL_TEXT, 7),
        FeatureMapping::new(b"pnum", FEATURE_TYPE_NUMBER_SPACING, FEATURE_SELECTOR_PROPORTIONAL_NUMBERS, 4),
        FeatureMapping::new(b"pwid", FEATURE_TYPE_TEXT_SPACING, FEATURE_SELECTOR_PROPORTIONAL_TEXT, 7),
        FeatureMapping::new(b"qwid", FEATURE_TYPE_TEXT_SPACING, FEATURE_SELECTOR_QUARTER_WIDTH_TEXT, 7),
        FeatureMapping::new(b"rlig", FEATURE_TYPE_LIGATURES, FEATURE_SELECTOR_REQUIRED_LIGATURES_ON, FEATURE_SELECTOR_REQUIRED_LIGATURES_OFF),
        FeatureMapping::new(b"ruby", FEATURE_TYPE_RUBY_KANA, FEATURE_SELECTOR_RUBY_KANA_ON, FEATURE_SELECTOR_RUBY_KANA_OFF),
        FeatureMapping::new(b"sinf", FEATURE_TYPE_VERTICAL_POSITION, FEATURE_SELECTOR_SCIENTIFIC_INFERIORS, FEATURE_SELECTOR_NORMAL_POSITION),
        FeatureMapping::new(b"smcp", FEATURE_TYPE_LOWER_CASE, FEATURE_SELECTOR_LOWER_CASE_SMALL_CAPS, FEATURE_SELECTOR_DEFAULT_LOWER_CASE),
        FeatureMapping::new(b"smpl", FEATURE_TYPE_CHARACTER_SHAPE, FEATURE_SELECTOR_SIMPLIFIED_CHARACTERS, 16),
        FeatureMapping::new(b"ss01", FEATURE_TYPE_STYLISTIC_ALTERNATIVES, FEATURE_SELECTOR_STYLISTIC_ALT_ONE_ON, FEATURE_SELECTOR_STYLISTIC_ALT_ONE_OFF),
        FeatureMapping::new(b"ss02", FEATURE_TYPE_STYLISTIC_ALTERNATIVES, FEATURE_SELECTOR_STYLISTIC_ALT_TWO_ON, FEATURE_SELECTOR_STYLISTIC_ALT_TWO_OFF),
        FeatureMapping::new(b"ss03", FEATURE_TYPE_STYLISTIC_ALTERNATIVES, FEATURE_SELECTOR_STYLISTIC_ALT_THREE_ON, FEATURE_SELECTOR_STYLISTIC_ALT_THREE_OFF),
        FeatureMapping::new(b"ss04", FEATURE_TYPE_STYLISTIC_ALTERNATIVES, FEATURE_SELECTOR_STYLISTIC_ALT_FOUR_ON, FEATURE_SELECTOR_STYLISTIC_ALT_FOUR_OFF),
        FeatureMapping::new(b"ss05", FEATURE_TYPE_STYLISTIC_ALTERNATIVES, FEATURE_SELECTOR_STYLISTIC_ALT_FIVE_ON, FEATURE_SELECTOR_STYLISTIC_ALT_FIVE_OFF),
        FeatureMapping::new(b"ss06", FEATURE_TYPE_STYLISTIC_ALTERNATIVES, FEATURE_SELECTOR_STYLISTIC_ALT_SIX_ON, FEATURE_SELECTOR_STYLISTIC_ALT_SIX_OFF),
        FeatureMapping::new(b"ss07", FEATURE_TYPE_STYLISTIC_ALTERNATIVES, FEATURE_SELECTOR_STYLISTIC_ALT_SEVEN_ON, FEATURE_SELECTOR_STYLISTIC_ALT_SEVEN_OFF),
        FeatureMapping::new(b"ss08", FEATURE_TYPE_STYLISTIC_ALTERNATIVES, FEATURE_SELECTOR_STYLISTIC_ALT_EIGHT_ON, FEATURE_SELECTOR_STYLISTIC_ALT_EIGHT_OFF),
        FeatureMapping::new(b"ss09", FEATURE_TYPE_STYLISTIC_ALTERNATIVES, FEATURE_SELECTOR_STYLISTIC_ALT_NINE_ON, FEATURE_SELECTOR_STYLISTIC_ALT_NINE_OFF),
        FeatureMapping::new(b"ss10", FEATURE_TYPE_STYLISTIC_ALTERNATIVES, FEATURE_SELECTOR_STYLISTIC_ALT_TEN_ON, FEATURE_SELECTOR_STYLISTIC_ALT_TEN_OFF),
        FeatureMapping::new(b"ss11", FEATURE_TYPE_STYLISTIC_ALTERNATIVES, FEATURE_SELECTOR_STYLISTIC_ALT_ELEVEN_ON, FEATURE_SELECTOR_STYLISTIC_ALT_ELEVEN_OFF),
        FeatureMapping::new(b"ss12", FEATURE_TYPE_STYLISTIC_ALTERNATIVES, FEATURE_SELECTOR_STYLISTIC_ALT_TWELVE_ON, FEATURE_SELECTOR_STYLISTIC_ALT_TWELVE_OFF),
        FeatureMapping::new(b"ss13", FEATURE_TYPE_STYLISTIC_ALTERNATIVES, FEATURE_SELECTOR_STYLISTIC_ALT_THIRTEEN_ON, FEATURE_SELECTOR_STYLISTIC_ALT_THIRTEEN_OFF),
        FeatureMapping::new(b"ss14", FEATURE_TYPE_STYLISTIC_ALTERNATIVES, FEATURE_SELECTOR_STYLISTIC_ALT_FOURTEEN_ON, FEATURE_SELECTOR_STYLISTIC_ALT_FOURTEEN_OFF),
        FeatureMapping::new(b"ss15", FEATURE_TYPE_STYLISTIC_ALTERNATIVES, FEATURE_SELECTOR_STYLISTIC_ALT_FIFTEEN_ON, FEATURE_SELECTOR_STYLISTIC_ALT_FIFTEEN_OFF),
        FeatureMapping::new(b"ss16", FEATURE_TYPE_STYLISTIC_ALTERNATIVES, FEATURE_SELECTOR_STYLISTIC_ALT_SIXTEEN_ON, FEATURE_SELECTOR_STYLISTIC_ALT_SIXTEEN_OFF),
        FeatureMapping::new(b"ss17", FEATURE_TYPE_STYLISTIC_ALTERNATIVES, FEATURE_SELECTOR_STYLISTIC_ALT_SEVENTEEN_ON, FEATURE_SELECTOR_STYLISTIC_ALT_SEVENTEEN_OFF),
        FeatureMapping::new(b"ss18", FEATURE_TYPE_STYLISTIC_ALTERNATIVES, FEATURE_SELECTOR_STYLISTIC_ALT_EIGHTEEN_ON, FEATURE_SELECTOR_STYLISTIC_ALT_EIGHTEEN_OFF),
        FeatureMapping::new(b"ss19", FEATURE_TYPE_STYLISTIC_ALTERNATIVES, FEATURE_SELECTOR_STYLISTIC_ALT_NINETEEN_ON, FEATURE_SELECTOR_STYLISTIC_ALT_NINETEEN_OFF),
        FeatureMapping::new(b"ss20", FEATURE_TYPE_STYLISTIC_ALTERNATIVES, FEATURE_SELECTOR_STYLISTIC_ALT_TWENTY_ON, FEATURE_SELECTOR_STYLISTIC_ALT_TWENTY_OFF),
        FeatureMapping::new(b"subs", FEATURE_TYPE_VERTICAL_POSITION, FEATURE_SELECTOR_INFERIORS, FEATURE_SELECTOR_NORMAL_POSITION),
        FeatureMapping::new(b"sups", FEATURE_TYPE_VERTICAL_POSITION, FEATURE_SELECTOR_SUPERIORS, FEATURE_SELECTOR_NORMAL_POSITION),
        FeatureMapping::new(b"swsh", FEATURE_TYPE_CONTEXTUAL_ALTERNATIVES, FEATURE_SELECTOR_SWASH_ALTERNATES_ON, FEATURE_SELECTOR_SWASH_ALTERNATES_OFF),
        FeatureMapping::new(b"titl", FEATURE_TYPE_STYLE_OPTIONS, FEATURE_SELECTOR_TITLING_CAPS, FEATURE_SELECTOR_NO_STYLE_OPTIONS),
        FeatureMapping::new(b"tnam", FEATURE_TYPE_CHARACTER_SHAPE, FEATURE_SELECTOR_TRADITIONAL_NAMES_CHARACTERS, 16),
        FeatureMapping::new(b"tnum", FEATURE_TYPE_NUMBER_SPACING, FEATURE_SELECTOR_MONOSPACED_NUMBERS, 4),
        FeatureMapping::new(b"trad", FEATURE_TYPE_CHARACTER_SHAPE, FEATURE_SELECTOR_TRADITIONAL_CHARACTERS, 16),
        FeatureMapping::new(b"twid", FEATURE_TYPE_TEXT_SPACING, FEATURE_SELECTOR_THIRD_WIDTH_TEXT, 7),
        FeatureMapping::new(b"unic", FEATURE_TYPE_LETTER_CASE, 14, 15),
        FeatureMapping::new(b"valt", FEATURE_TYPE_TEXT_SPACING, FEATURE_SELECTOR_ALT_PROPORTIONAL_TEXT, 7),
        FeatureMapping::new(b"vert", FEATURE_TYPE_VERTICAL_SUBSTITUTION, FEATURE_SELECTOR_SUBSTITUTE_VERTICAL_FORMS_ON, FEATURE_SELECTOR_SUBSTITUTE_VERTICAL_FORMS_OFF),
        FeatureMapping::new(b"vhal", FEATURE_TYPE_TEXT_SPACING, FEATURE_SELECTOR_ALT_HALF_WIDTH_TEXT, 7),
        FeatureMapping::new(b"vkna", FEATURE_TYPE_ALTERNATE_KANA, FEATURE_SELECTOR_ALTERNATE_VERT_KANA_ON, FEATURE_SELECTOR_ALTERNATE_VERT_KANA_OFF),
        FeatureMapping::new(b"vpal", FEATURE_TYPE_TEXT_SPACING, FEATURE_SELECTOR_ALT_PROPORTIONAL_TEXT, 7),
        FeatureMapping::new(b"vrt2", FEATURE_TYPE_VERTICAL_SUBSTITUTION, FEATURE_SELECTOR_SUBSTITUTE_VERTICAL_FORMS_ON, FEATURE_SELECTOR_SUBSTITUTE_VERTICAL_FORMS_OFF),
        FeatureMapping::new(b"vrtr", FEATURE_TYPE_VERTICAL_SUBSTITUTION, 2, 3),
        FeatureMapping::new(b"zero", FEATURE_TYPE_TYPOGRAPHIC_EXTRAS, FEATURE_SELECTOR_SLASHED_ZERO_ON, FEATURE_SELECTOR_SLASHED_ZERO_OFF),
    ];

    pub const DELETED_GLYPH: u32 = 0xFFFF;

    /// HB: hb_aat_layout_substitute
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/main/src/hb-aat-layout.cc#L285>
    #[doc(alias = "hb_aat_layout_substitute")]
    pub fn substitute(
        plan: &ShapePlan,
        layout: LayoutData<'_>,
        buffer: &mut Buffer,
        features: &[Feature],
        #[cfg(feature = "trace")] font: &crate::ShaperFont<'_, '_>,
    ) {
        let mut aat_map = map::AatMap::default();
        if !features.is_empty() {
            let mut builder = map::AatMapBuilder::new(plan.language.as_ref());
            for feature in features {
                builder.add_feature(layout.aat, feature);
            }
            builder.compile(layout.aat, &mut aat_map);
        }

        let mut c = AatApplyContext::new(
            plan,
            layout,
            Scale::default(),
            buffer,
            #[cfg(feature = "trace")]
            font,
        );
        #[cfg(feature = "trace")]
        let table = if layout.aat.morx.is_some() {
            "morx"
        } else {
            "mort"
        };
        message_return!(c, "start table {table}");
        morx::apply(
            &mut c,
            if features.is_empty() {
                &plan.aat_map
            } else {
                &aat_map
            },
        );
        message!(c, "end table {table}");
    }

    fn is_deleted_glyph(info: &GlyphInfo) -> bool {
        info.is_aat_deleted()
    }

    /// HB: hb_aat_layout_remove_deleted_glyphs
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/2c22a65f0cb99544c36580b9703a43b5dc97a9e1/src/hb-aat-layout.cc#L337>
    #[doc(alias = "hb_aat_layout_remove_deleted_glyphs")]
    pub fn remove_deleted_glyphs(buffer: &mut Buffer) {
        if (buffer.scratch_flags & HB_BUFFER_SCRATCH_FLAG_AAT_HAS_DELETED) != 0 {
            buffer.delete_glyphs_inplace(is_deleted_glyph);
        }
    }

    /// HB: hb_aat_layout_position
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/2c22a65f0cb99544c36580b9703a43b5dc97a9e1/src/hb-aat-layout.cc#L363>
    #[doc(alias = "hb_aat_layout_position")]
    pub fn position(
        plan: &ShapePlan,
        layout: LayoutData<'_>,
        scale: Scale,
        buffer: &mut Buffer,
        #[cfg(feature = "trace")] font: &crate::ShaperFont<'_, '_>,
    ) {
        let mut c = AatApplyContext::new(
            plan,
            layout,
            scale,
            buffer,
            #[cfg(feature = "trace")]
            font,
        );
        message_return!(c, "start table kerx");
        kerx::apply(&mut c);
        message!(c, "end table kerx");
    }

    /// HB: hb_aat_layout_track
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/2c22a65f0cb99544c36580b9703a43b5dc97a9e1/src/hb-aat-layout.cc#L398>
    #[doc(alias = "hb_aat_layout_track")]
    pub fn track(
        plan: &ShapePlan,
        layout: LayoutData<'_>,
        scale: Scale,
        point_size: Option<f32>,
        buffer: &mut Buffer,
    ) {
        trak::apply(plan, layout, scale, point_size, buffer);
    }
}
pub mod map {
    use crate::Feature;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::cmp::Ordering;

    use super::layout::*;
    use super::AatData;
    use crate::{Language, Mask, Tag};

    /// HB: hb_aat_map_t
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/2c22a65f0cb99544c36580b9703a43b5dc97a9e1/src/hb-aat-map.hh#L33>
    #[doc(alias = "hb_aat_map_t")]
    #[derive(Default)]
    pub struct AatMap {
        pub chain_flags: Vec<Vec<RangeFlags>>,
    }

    /// HB: hb_aat_map_t::range_flags_t
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/2c22a65f0cb99544c36580b9703a43b5dc97a9e1/src/hb-aat-map.hh#L38>
    #[derive(Copy, Clone)]
    pub struct RangeFlags {
        pub flags: Mask,
        pub cluster_first: u32,
        pub cluster_last: u32, // end - 1
    }

    /// HB: hb_aat_map_builder_t
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/2c22a65f0cb99544c36580b9703a43b5dc97a9e1/src/hb-aat-map.hh#L49>
    #[doc(alias = "hb_aat_map_builder_t")]
    pub struct AatMapBuilder {
        pub language: Option<Language>,
        pub current_features: Vec<FeatureInfo>,
        pub features: Vec<FeatureRange>,
        pub range_first: usize,
        pub range_last: usize,
    }

    impl Default for AatMapBuilder {
        fn default() -> Self {
            Self {
                language: None,
                range_first: Feature::GLOBAL_START as usize,
                range_last: Feature::GLOBAL_END as usize,
                current_features: Vec::default(),
                features: Vec::default(),
            }
        }
    }

    impl AatMapBuilder {
        pub fn new(language: Option<&Language>) -> Self {
            Self {
                language: language.cloned(),
                ..Self::default()
            }
        }

        pub fn add_feature(&mut self, aat: &AatData, feature: &Feature) -> Option<()> {
            let feat = aat.feat.as_ref()?;

            if feature.tag == Tag::new(b"aalt") {
                let exposes_feature = feat
                    .find(FEATURE_TYPE_CHARACTER_ALTERNATIVES as u16)
                    .is_some_and(|f| f.n_settings() != 0);

                if !exposes_feature {
                    return Some(());
                }

                self.features.push(FeatureRange {
                    start: feature.start,
                    end: feature.end,
                    info: FeatureInfo {
                        kind: FEATURE_TYPE_CHARACTER_ALTERNATIVES as u16,
                        setting: u16::try_from(feature.value).unwrap(),
                        is_exclusive: true,
                    },
                });
            }

            let idx = feature_mappings
                .binary_search_by(|map| map.ot_feature_tag.cmp(&feature.tag))
                .ok()?;
            let mapping = &feature_mappings[idx];

            let mut feature_name = feat.find(mapping.aat_feature_type as u16);

            match feature_name {
                Some(feature) if feature.n_settings() != 0 => {}
                _ => {
                    // Special case: Chain::compile_flags will fall back to the deprecated version of
                    // small-caps if necessary, so we need to check for that possibility.
                    // https://github.com/harfbuzz/harfbuzz/issues/2307
                    if mapping.aat_feature_type == FEATURE_TYPE_LOWER_CASE
                        && mapping.selector_to_enable == FEATURE_SELECTOR_LOWER_CASE_SMALL_CAPS
                    {
                        feature_name = feat.find(FEATURE_TYPE_LETTER_CASE as u16);
                    }
                }
            }

            match feature_name {
                Some(feature_name) if feature_name.n_settings() != 0 => {
                    let setting = if feature.value != 0 {
                        mapping.selector_to_enable
                    } else {
                        mapping.selector_to_disable
                    } as u16;

                    self.features.push(FeatureRange {
                        start: feature.start,
                        end: feature.end,
                        info: FeatureInfo {
                            kind: mapping.aat_feature_type as u16,
                            setting,
                            is_exclusive: feature_name.is_exclusive(),
                        },
                    });
                }
                _ => {}
            }

            Some(())
        }

        pub fn compile(&mut self, aat: &AatData, m: &mut AatMap) {
            // Compute active features per range, and compile each.
            let mut feature_events = vec![];
            for feature in &self.features {
                if feature.start == feature.end {
                    continue;
                }

                feature_events.push(FeatureEvent {
                    index: feature.start as usize,
                    start: true,
                    seq: feature_events.len(),
                    feature: feature.info,
                });

                feature_events.push(FeatureEvent {
                    index: feature.end as usize,
                    start: false,
                    seq: feature_events.len(),
                    feature: feature.info,
                });
            }

            crate::algs::sort_unstable_by(&mut feature_events, Ord::cmp);

            // Add a strategic final event.
            feature_events.push(FeatureEvent {
                index: u32::MAX as usize,
                start: false,
                seq: feature_events.len(),
                feature: FeatureInfo::default(),
            });

            // Scan events and save features for each range.
            let mut active_features = vec![];
            let mut last_index = 0;

            for event in &feature_events {
                if event.index != last_index {
                    // Save a snapshot of active features and the range.
                    // Sort features and merge duplicates.
                    self.current_features.clone_from(&active_features);
                    self.range_first = last_index;
                    self.range_last = event.index.wrapping_sub(1);

                    if !self.current_features.is_empty() {
                        self.current_features.sort();
                        let mut j = 0;
                        for i in 1..self.current_features.len() {
                            // Nonexclusive feature selectors come in even/odd pairs to turn a setting on/off
                            // respectively, so we mask out the low-order bit when checking for "duplicates"
                            // (selectors referring to the same feature setting) here.
                            let non_exclusive = !self.current_features[i].is_exclusive
                                && (self.current_features[i].setting & !1)
                                    != (self.current_features[j].setting & !1);

                            if self.current_features[i].kind != self.current_features[j].kind
                                || non_exclusive
                            {
                                j += 1;
                                self.current_features[j] = self.current_features[i];
                            }
                        }
                        self.current_features.truncate(j + 1);
                    }

                    super::morx::compile_flags(aat, self, m);
                    last_index = event.index;
                }

                if event.start {
                    active_features.push(event.feature);
                } else {
                    if let Some(index) = active_features.iter().position(|&f| f == event.feature) {
                        active_features.remove(index);
                    }
                }
            }

            for chain_flags in &mut m.chain_flags {
                if let Some(last) = chain_flags.last_mut() {
                    last.cluster_last = Feature::GLOBAL_END;
                }
            }
        }
    }

    /// HB: hb_aat_map_builder_t::FeatureInfo
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/2c22a65f0cb99544c36580b9703a43b5dc97a9e1/src/hb-aat-map.hh#L63>
    #[derive(Copy, Clone, PartialEq, Eq, Default)]
    pub struct FeatureInfo {
        pub kind: u16,
        pub setting: u16,
        pub is_exclusive: bool,
    }

    impl Ord for FeatureInfo {
        fn cmp(&self, other: &Self) -> Ordering {
            self.partial_cmp(other).unwrap()
        }
    }

    impl PartialOrd for FeatureInfo {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            if self.kind != other.kind {
                Some(self.kind.cmp(&other.kind))
            } else if !self.is_exclusive && (self.setting & !1) != (other.setting & !1) {
                Some(self.setting.cmp(&other.setting))
            } else {
                Some(Ordering::Equal)
            }
        }
    }

    /// HB: hb_aat_map_builder_t::feature_range_t
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/2c22a65f0cb99544c36580b9703a43b5dc97a9e1/src/hb-aat-map.hh#L88>
    #[derive(Copy, Clone, PartialEq, Eq)]
    pub struct FeatureRange {
        pub info: FeatureInfo,
        pub start: u32,
        pub end: u32,
    }

    /// HB: hb_aat_map_builder_t::feature_event_t
    ///
    /// See <https://github.com/harfbuzz/harfbuzz/blob/2c22a65f0cb99544c36580b9703a43b5dc97a9e1/src/hb-aat-map.hh#L96>
    #[derive(Copy, Clone, Eq, PartialEq)]
    struct FeatureEvent {
        pub index: usize,
        pub start: bool,
        /// Order of addition, which breaks ties between otherwise equal events.
        pub seq: usize,
        pub feature: FeatureInfo,
    }

    impl Ord for FeatureEvent {
        fn cmp(&self, other: &Self) -> Ordering {
            self.partial_cmp(other).unwrap()
        }
    }

    impl PartialOrd for FeatureEvent {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            if self.index != other.index {
                Some(self.index.cmp(&other.index))
            } else if self.start != other.start {
                Some(self.start.cmp(&other.start))
            } else {
                Some(self.seq.cmp(&other.seq))
            }
        }
    }
}
pub mod morx;
pub mod trak {
    use read_fonts::tables::trak::TrackTableEntry;
    use read_fonts::types::{BigEndian, Fixed};
    use read_fonts::FontData;

    use crate::LayoutData;
    use crate::{buffer::Buffer, plan::ShapePlan, Scale};

    pub fn apply(
        _plan: &ShapePlan,
        layout: LayoutData<'_>,
        scale: Scale,
        point_size: Option<f32>,
        buffer: &mut Buffer,
    ) -> Option<()> {
        let trak = layout.aat.trak.as_ref()?;
        let mut ptem = point_size.unwrap_or(0.0);
        if ptem <= 0.0 {
            ptem = 12.0; // CoreText fallback
        }

        if !buffer.have_positions {
            buffer.clear_positions();
        }

        let advance_to_add = if buffer.direction.is_horizontal() {
            scale.scale_x_f(trak.get_h_tracking(ptem, 0.0))
        } else {
            scale.scale_y_f(trak.get_v_tracking(ptem, 0.0))
        };

        foreach_grapheme!(buffer, start, end, {
            if let Some(pos) = buffer.pos.get_mut(start) {
                if buffer.direction.is_horizontal() {
                    pos.x_advance = pos.x_advance.saturating_add(advance_to_add);
                } else {
                    pos.y_advance = pos.y_advance.saturating_add(advance_to_add);
                }
            }
        });

        Some(())
    }

    trait TrakExt {
        fn get_h_tracking(&self, ptem: f32, track: f32) -> f32;
        fn get_v_tracking(&self, ptem: f32, track: f32) -> f32;
    }

    impl TrakExt for read_fonts::tables::trak::Trak<'_> {
        // Keep tracking fractional; scale_x_f rounds once, matching HarfBuzz's
        // em_scalef (the interpolated value is generally not a whole font unit).
        fn get_h_tracking(&self, ptem: f32, track: f32) -> f32 {
            self.horiz()
                .transpose()
                .ok()
                .flatten()
                .map_or(0.0, |t| t.get_tracking(self.offset_data(), ptem, track))
        }

        fn get_v_tracking(&self, ptem: f32, track: f32) -> f32 {
            self.vert()
                .transpose()
                .ok()
                .flatten()
                .map_or(0.0, |t| t.get_tracking(self.offset_data(), ptem, track))
        }
    }

    trait TrackDataExt {
        fn get_tracking(&self, offset_data: FontData, ptem: f32, track: f32) -> f32;
    }

    impl TrackDataExt for read_fonts::tables::trak::TrackData<'_> {
        fn get_tracking(&self, offset_data: FontData, ptem: f32, track: f32) -> f32 {
            let Some(sizes) = self.size_table(offset_data) else {
                return 0.0;
            };

            if sizes.is_empty() {
                return 0.0;
            }

            let tracks = self.track_table();
            if tracks.is_empty() {
                return 0.0;
            }

            let get_value = |entry: &TrackTableEntry| {
                let Some(values) = entry.per_size_values(offset_data, sizes.len() as u16) else {
                    return 0.0;
                };
                entry.get_value(ptem, sizes, values)
            };

            if tracks.len() == 1 {
                return tracks.first().map_or(0.0, get_value);
            }

            let mut i = 0;
            let mut j = tracks.len() - 1;

            while i + 1 < tracks.len()
                && tracks.get(i + 1).map_or(0.0, |t| t.track().to_f32()) <= track
            {
                i += 1;
            }
            while j > 0 && tracks.get(j - 1).map_or(0.0, |t| t.track().to_f32()) >= track {
                j -= 1;
            }

            if i == j {
                return tracks.get(i).map_or(0.0, get_value);
            }

            let t0 = tracks.get(i).map_or(0.0, |t| t.track().to_f32());
            let t1 = tracks.get(j).map_or(0.0, |t| t.track().to_f32());
            let interp = if (t1 - t0).abs() < f32::EPSILON {
                0.0
            } else {
                (track - t0) / (t1 - t0)
            };

            let a = tracks.get(i).map_or(0.0, get_value);
            let b = tracks.get(j).map_or(0.0, get_value);
            a + interp * (b - a)
        }
    }

    trait TrackEntryExt {
        fn get_value(
            &self,
            ptem: f32,
            sizes: &[BigEndian<Fixed>],
            values: &[BigEndian<i16>],
        ) -> f32;
    }

    impl TrackEntryExt for TrackTableEntry {
        fn get_value(
            &self,
            ptem: f32,
            sizes: &[BigEndian<Fixed>],
            values: &[BigEndian<i16>],
        ) -> f32 {
            let n = sizes.len().min(values.len());
            if n == 0 {
                return 0.0;
            }

            for i in 0..n {
                let size_pt = sizes.get(i).map_or(0.0, |f| f.get().to_f32());
                if size_pt >= ptem {
                    if i == 0 {
                        return values.first().map(|v| v.get() as f32).unwrap_or_default();
                    }

                    let s0 = sizes.get(i - 1).map_or(0.0, |f| f.get().to_f32());
                    let s1 = size_pt;
                    let v0 = values
                        .get(i - 1)
                        .map(|v| v.get() as f32)
                        .unwrap_or_default();
                    let v1 = values.get(i).map(|v| v.get() as f32).unwrap_or_default();

                    if (s1 - s0).abs() < f32::EPSILON {
                        return f32::midpoint(v0, v1);
                    }

                    let t = (ptem - s0) / (s1 - s0);
                    return v0 + t * (v1 - v0);
                }
            }

            values
                .get(n - 1)
                .map(|v| v.get() as f32)
                .unwrap_or_default()
        }
    }
}

use crate::aat::common::SafeToBreakAccel;
use crate::aat::kern::KernSubtableCache;
use crate::aat::kerx::KerxSubtableCache;
use crate::aat::morx::{MorphSubtableCache, MorphSubtableDescriptor};
use crate::once::Once;
use crate::ot::OtCache;
use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use read_fonts::{
    model::Tables,
    tables::{
        ankr::Ankr, feat::Feat, kern::Kern, kerx::Kerx, ltag::Ltag, mort::Mort, morx::Morx,
        trak::Trak,
    },
    TableProvider,
};
use yoke::{Yoke, Yokeable};

/// Font-wide AAT tables and accelerators initialized on first use.
pub struct AatCache {
    // Keep uninitialized caches small, including in fonts without AAT tables.
    morx: Once<Box<Yoke<MorphTableCache<'static>, Arc<Tables>>>>,
    mort: Once<Box<Yoke<MorphTableCache<'static>, Arc<Tables>>>>,
    kern: Once<Box<Yoke<KernTableCache<'static>, Arc<Tables>>>>,
    kerx: Once<Box<Yoke<KerxTableCache<'static>, Arc<Tables>>>>,
    tables: Option<Yoke<CachedAatTables<'static>, Arc<Tables>>>,
}

impl AatCache {
    pub fn new(tables: Option<Arc<Tables>>, ot_cache: &OtCache) -> Self {
        let active_gdef_len = if ot_cache.has_gdef {
            ot_cache.gdef_len
        } else {
            0
        };
        let gsub_len = ot_cache.gsub_len;
        Self {
            morx: Once::new(),
            mort: Once::new(),
            kern: Once::new(),
            kerx: Once::new(),
            tables: tables.map(|tables| {
                Yoke::attach_to_cart(tables, |tables| {
                    CachedAatTables::new(tables, gsub_len, active_gdef_len)
                })
            }),
        }
    }
}

/// The AAT tables, parsed once and kept with the font tables.
#[derive(Yokeable)]
pub struct CachedAatTables<'a> {
    pub morx: Option<Morx<'a>>,
    pub mort: Option<Mort<'a>>,
    pub ankr: Option<Ankr<'a>>,
    pub kern: Option<Kern<'a>>,
    pub kerx: Option<Kerx<'a>>,
    pub trak: Option<Trak<'a>>,
    pub feat: Option<Feat<'a>>,
    pub ltag: Option<Ltag<'a>>,
    num_glyphs: u32,
}

impl<'a> CachedAatTables<'a> {
    const EMPTY: Self = Self {
        morx: None,
        mort: None,
        ankr: None,
        kern: None,
        kerx: None,
        trak: None,
        feat: None,
        ltag: None,
        num_glyphs: 0,
    };

    fn new(tables: &'a Tables, gsub_len: u32, gdef_len: u32) -> Self {
        let morx = tables.morx().ok().filter(|morx| {
            let morx_len = morx.offset_data().len() as u32;
            !is_morx_blocklisted(morx_len, gsub_len, gdef_len)
        });
        Self {
            morx,
            mort: tables.mort().ok(),
            ankr: tables.ankr().ok(),
            kern: tables.kern().ok(),
            kerx: tables.kerx().ok(),
            trak: tables.trak().ok(),
            feat: tables.feat().ok(),
            ltag: tables.ltag().ok(),
            num_glyphs: tables
                .selected_maxp()
                .map(|maxp| maxp.num_glyphs())
                .unwrap_or_default(),
        }
    }
}

/// The subtables of a `morx` or `mort` table, parsed, with their
/// accelerators.
#[derive(Default, Yokeable)]
pub(crate) struct MorphTableCache<'a> {
    safe_to_break: SafeToBreakAccel,
    subtables: Vec<MorphSubtableCache<'a>>,
    descriptors: Vec<MorphSubtableDescriptor>,
}

impl<'a> MorphTableCache<'a> {
    fn new_morx(morx: &Morx<'a>, num_glyphs: u32) -> Self {
        let mut cache = Self::default();
        for (chain_index, chain) in morx.chains().iter().enumerate() {
            let Ok(chain) = chain else {
                continue;
            };
            for subtable in chain.subtables().iter() {
                let Ok(subtable) = subtable else {
                    continue;
                };
                let entry =
                    MorphSubtableCache::new_morx(&subtable, num_glyphs, &mut cache.safe_to_break);
                cache
                    .descriptors
                    .push(MorphSubtableCache::descriptor_morx(chain_index, &subtable));
                cache.subtables.push(entry);
            }
        }
        cache
    }

    fn new_mort(mort: &Mort<'a>, num_glyphs: u32) -> Self {
        let mut cache = Self::default();
        for (chain_index, chain) in mort.chains().iter().enumerate() {
            let Ok(chain) = chain else {
                continue;
            };
            for subtable in chain.subtables().iter() {
                let Ok(subtable) = subtable else {
                    continue;
                };
                let entry =
                    MorphSubtableCache::new_mort(&subtable, num_glyphs, &mut cache.safe_to_break);
                cache
                    .descriptors
                    .push(MorphSubtableCache::descriptor_mort(chain_index, &subtable));
                cache.subtables.push(entry);
            }
        }
        cache
    }
}

/// The subtables of a `kern` table, parsed, with their accelerators.
#[derive(Default, Yokeable)]
pub(crate) struct KernTableCache<'a> {
    safe_to_break: SafeToBreakAccel,
    subtables: Vec<KernSubtableCache<'a>>,
}

impl<'a> KernTableCache<'a> {
    fn new(kern: &Kern<'a>, num_glyphs: u32) -> Self {
        let mut cache = Self::default();
        for subtable in kern.subtables() {
            let Ok(subtable) = subtable else {
                continue;
            };
            let entry = KernSubtableCache::new(&subtable, num_glyphs, &mut cache.safe_to_break);
            cache.subtables.push(entry);
        }
        cache
    }
}

/// The subtables of a `kerx` table, parsed, with their accelerators.
#[derive(Default, Yokeable)]
pub(crate) struct KerxTableCache<'a> {
    safe_to_break: SafeToBreakAccel,
    subtables: Vec<KerxSubtableCache<'a>>,
}

impl<'a> KerxTableCache<'a> {
    fn new(kerx: &Kerx<'a>, num_glyphs: u32) -> Self {
        let mut cache = Self::default();
        for subtable in kerx.subtables().iter() {
            let Ok(subtable) = subtable else {
                continue;
            };
            let entry = KerxSubtableCache::new(&subtable, num_glyphs, &mut cache.safe_to_break);
            cache.subtables.push(entry);
        }
        cache
    }
}

/// The AAT tables of a font, and its accelerators.
#[derive(Clone)]
pub struct AatData<'a> {
    cache: Option<&'a AatCache>,
    tables: &'a CachedAatTables<'a>,
}

impl<'a> core::ops::Deref for AatData<'a> {
    type Target = CachedAatTables<'a>;

    fn deref(&self) -> &Self::Target {
        self.tables
    }
}

use crate::algs::HB_CODEPOINT_ENCODE3 as encode3;

/// Blocklist specific broken morx tables identified by the combination of
/// morx, GSUB, and GDEF table lengths.
fn is_morx_blocklisted(morx_len: u32, gsub_len: u32, gdef_len: u32) -> bool {
    const BLOCKLIST: &[u64] = &[
        // AALMAGHRIBI.ttf — https://github.com/harfbuzz/harfbuzz/issues/4108
        encode3(19892, 2794, 340),
    ];
    let key = encode3(morx_len, gsub_len, gdef_len);
    BLOCKLIST.contains(&key)
}

static EMPTY_AAT_TABLES: CachedAatTables<'static> = CachedAatTables::EMPTY;

pub(crate) static EMPTY_AAT_DATA: AatData<'static> = AatData {
    cache: None,
    tables: &EMPTY_AAT_TABLES,
};

impl<'a> AatData<'a> {
    pub fn new(cache: &'a AatCache) -> Self {
        Self {
            cache: Some(cache),
            tables: cache.tables.as_ref().map_or(&EMPTY_AAT_TABLES, Yoke::get),
        }
    }

    // Access accelerators only when applying the corresponding table, so
    // planning can select GPOS or a preferred AAT table without building them.
    // The accelerators borrow the parsed tables, so each is projected from
    // the yoke that holds them.
    pub(crate) fn morx_cache(&self) -> Option<&'a MorphTableCache<'a>> {
        self.morx.as_ref()?;
        let cache = self.cache?;
        let tables = cache.tables.as_ref()?;
        let yoke = cache.morx.get_or_init(|| {
            Box::new(tables.map_project_cloned(|tables, _| {
                tables
                    .morx
                    .as_ref()
                    .map(|morx| MorphTableCache::new_morx(morx, tables.num_glyphs))
                    .unwrap_or_default()
            }))
        });
        Some(yoke.get())
    }

    pub(crate) fn mort_cache(&self) -> Option<&'a MorphTableCache<'a>> {
        self.mort.as_ref()?;
        let cache = self.cache?;
        let tables = cache.tables.as_ref()?;
        let yoke = cache.mort.get_or_init(|| {
            Box::new(tables.map_project_cloned(|tables, _| {
                tables
                    .mort
                    .as_ref()
                    .map(|mort| MorphTableCache::new_mort(mort, tables.num_glyphs))
                    .unwrap_or_default()
            }))
        });
        Some(yoke.get())
    }

    pub(crate) fn kern_cache(&self) -> Option<&'a KernTableCache<'a>> {
        self.kern.as_ref()?;
        let cache = self.cache?;
        let tables = cache.tables.as_ref()?;
        let yoke = cache.kern.get_or_init(|| {
            Box::new(tables.map_project_cloned(|tables, _| {
                tables
                    .kern
                    .as_ref()
                    .map(|kern| KernTableCache::new(kern, tables.num_glyphs))
                    .unwrap_or_default()
            }))
        });
        Some(yoke.get())
    }

    pub(crate) fn kerx_cache(&self) -> Option<&'a KerxTableCache<'a>> {
        self.kerx.as_ref()?;
        let cache = self.cache?;
        let tables = cache.tables.as_ref()?;
        let yoke = cache.kerx.get_or_init(|| {
            Box::new(tables.map_project_cloned(|tables, _| {
                tables
                    .kerx
                    .as_ref()
                    .map(|kerx| KerxTableCache::new(kerx, tables.num_glyphs))
                    .unwrap_or_default()
            }))
        });
        Some(yoke.get())
    }
}
