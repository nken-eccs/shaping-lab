pub(crate) mod hangul {
    use alloc::boxed::Box;

    use crate::normalize::NormalizationMode;
    use crate::ot::map::*;
    use crate::ot::shaper::*;
    use crate::plan::ShapePlan;
    use crate::unicode::Codepoint;
    use crate::BufferFlags;
    use crate::Mask;
    use crate::ShaperFont;

    const LJMO: u8 = 1;
    const VJMO: u8 = 2;
    const TJMO: u8 = 3;

    impl GlyphInfo {
        declare_buffer_var_alias!(
            OT_SHAPER_VAR_U8_AUXILIARY_VAR,
            u8,
            HANGUL_SHAPING_FEATURE_VAR,
            hangul_shaping_feature,
            set_hangul_shaping_feature
        );
    }

    fn collect_features_hangul(planner: &mut ShapePlanner) {
        planner.ot_map.add_feature(Tag::new(b"ljmo"), F_NONE, 1);
        planner.ot_map.add_feature(Tag::new(b"vjmo"), F_NONE, 1);
        planner.ot_map.add_feature(Tag::new(b"tjmo"), F_NONE, 1);
    }

    fn override_features_hangul(planner: &mut ShapePlanner) {
        // Uniscribe does not apply 'calt' for Hangul, and certain fonts
        // (Noto Sans CJK, Source Han Sans, etc) apply all of jamo lookups
        // in calt, which is not desirable.
        //
        // Rather than disabling 'calt' for the entire run, which also turns
        // it off for any other characters in the run, allocate a mask for it
        // and clear that mask on jamo only (in setup_masks_hangul). That
        // keeps jamo out of reach of such lookups while letting the rest of
        // the run shape with 'calt' as usual.
        //
        // https://github.com/harfbuzz/harfbuzz/discussions/4853
        planner.ot_map.add_feature(Tag::new(b"calt"), F_NONE, 1);
    }

    struct HangulShapePlan {
        mask_array: [Mask; 4],
        calt_mask: Mask,
    }

    fn data_create_hangul(map: &OtMap) -> HangulShapePlan {
        HangulShapePlan {
            mask_array: [
                0,
                map.get_1_mask(Tag::new(b"ljmo")),
                map.get_1_mask(Tag::new(b"vjmo")),
                map.get_1_mask(Tag::new(b"tjmo")),
            ],
            calt_mask: map.get_1_mask(Tag::new(b"calt")),
        }
    }

    const L_BASE: u32 = 0x1100;
    const V_BASE: u32 = 0x1161;
    const T_BASE: u32 = 0x11A7;
    const L_COUNT: u32 = 19;
    const V_COUNT: u32 = 21;
    const T_COUNT: u32 = 28;
    const N_COUNT: u32 = V_COUNT * T_COUNT;
    const S_COUNT: u32 = L_COUNT * N_COUNT;
    const S_BASE: u32 = 0xAC00;

    fn is_combining_l(u: u32) -> bool {
        (L_BASE..L_BASE + L_COUNT).contains(&u)
    }

    fn is_combining_v(u: u32) -> bool {
        (V_BASE..V_BASE + V_COUNT).contains(&u)
    }

    fn is_combining_t(u: u32) -> bool {
        (T_BASE + 1..T_BASE + T_COUNT).contains(&u)
    }

    fn is_combined_s(u: u32) -> bool {
        (S_BASE..S_BASE + S_COUNT).contains(&u)
    }

    fn is_l(u: u32) -> bool {
        (0x1100..=0x115F).contains(&u) || (0xA960..=0xA97C).contains(&u)
    }

    fn is_v(u: u32) -> bool {
        (0x1160..=0x11A7).contains(&u) || (0xD7B0..=0xD7C6).contains(&u)
    }

    fn is_t(u: u32) -> bool {
        (0x11A8..=0x11FF).contains(&u) || (0xD7CB..=0xD7FB).contains(&u)
    }

    fn is_hangul_tone(u: u32) -> bool {
        (0x302E..=0x302F).contains(&u)
    }

    fn is_zero_width_char(font: &ShaperFont<'_, '_>, c: Codepoint) -> bool {
        if let Some(glyph) = font.nominal_glyph(c) {
            font.glyph_h_advance(glyph) == 0
        } else {
            false
        }
    }

    fn preprocess_text_hangul(
        _: &ShapePlan,
        font: &ShaperFont<'_, '_>,
        buffer: &mut Buffer,
    ) -> Option<()> {
        buffer.allocate_var(GlyphInfo::HANGUL_SHAPING_FEATURE_VAR);

        // Hangul syllables come in two shapes: LV, and LVT.  Of those:
        //
        //   - LV can be precomposed, or decomposed.  Lets call those
        //     <LV> and <L,V>,
        //   - LVT can be fully precomposed, partially precomposed, or
        //     fully decomposed.  Ie. <LVT>, <LV,T>, or <L,V,T>.
        //
        // The composition / decomposition is mechanical.  However, not
        // all <L,V> sequences compose, and not all <LV,T> sequences
        // compose.
        //
        // Here are the specifics:
        //
        //   - <L>: U+1100..115F, U+A960..A97F
        //   - <V>: U+1160..11A7, U+D7B0..D7C7
        //   - <T>: U+11A8..11FF, U+D7CB..D7FB
        //
        //   - Only the <L,V> sequences for some of the U+11xx ranges combine.
        //   - Only <LV,T> sequences for some of the Ts in U+11xx range combine.
        //
        // Here is what we want to accomplish in this shaper:
        //
        //   - If the whole syllable can be precomposed, do that,
        //   - Otherwise, fully decompose and apply ljmo/vjmo/tjmo features.
        //   - If a valid syllable is followed by a Hangul tone mark, reorder the tone
        //     mark to precede the whole syllable - unless it is a zero-width glyph, in
        //     which case we leave it untouched, assuming it's designed to overstrike.
        //
        // That is, of the different possible syllables:
        //
        //   <L>
        //   <L,V>
        //   <L,V,T>
        //   <LV>
        //   <LVT>
        //   <LV, T>
        //
        // - <L> needs no work.
        //
        // - <LV> and <LVT> can stay the way they are if the font supports them, otherwise we
        //   should fully decompose them if font supports.
        //
        // - <L,V> and <L,V,T> we should compose if the whole thing can be composed.
        //
        // - <LV,T> we should compose if the whole thing can be composed, otherwise we should
        //   decompose.

        buffer.clear_output();
        // Extent of most recently seen syllable; valid only if start < end
        let mut start = 0;
        let mut end = 0;
        buffer.idx = 0;
        while buffer.idx < buffer.len {
            let u = buffer.cur(0)?.glyph_id;
            let c = buffer.cur(0)?.as_codepoint();

            if is_hangul_tone(u) {
                // We could cache the width of the tone marks and the existence of dotted-circle,
                // but the use of the Hangul tone mark characters seems to be rare enough that
                // I didn't bother for now.
                if start < end && end == buffer.out_len {
                    // Tone mark follows a valid syllable; move it in front, unless it's zero width.
                    buffer.unsafe_to_break_from_outbuffer(Some(start), Some(buffer.idx));
                    buffer.next_glyph();
                    if !is_zero_width_char(font, c) {
                        buffer.merge_out_clusters(start, end + 1);
                        if let Some(syllable) = buffer.out_info_mut().get_mut(start..=end) {
                            syllable.rotate_right(1);
                        }
                    }
                } else {
                    // No valid syllable as base for tone mark; try to insert dotted circle.
                    if !buffer
                        .flags
                        .contains(BufferFlags::DO_NOT_INSERT_DOTTED_CIRCLE)
                        && font.has_glyph(0x25CC)
                    {
                        let mut chars = [0; 2];
                        if !is_zero_width_char(font, c) {
                            chars[0] = u;
                            chars[1] = 0x25CC;
                        } else {
                            chars[0] = 0x25CC;
                            chars[1] = u;
                        }

                        buffer.replace_glyphs(1, 2, &chars);
                    } else {
                        // No dotted circle available in the font; just leave tone mark untouched.
                        buffer.next_glyph();
                    }
                }

                start = buffer.out_len;
                end = buffer.out_len;
                continue;
            }

            // Remember current position as a potential syllable start;
            // will only be used if we set end to a later position.
            start = buffer.out_len;

            if is_l(u) && buffer.idx + 1 < buffer.len {
                let l = u;
                let v = buffer.cur(1)?.glyph_id;
                if is_v(v) {
                    // Have <L,V> or <L,V,T>.
                    let mut t = 0;
                    let mut tindex = 0;
                    if buffer.idx + 2 < buffer.len {
                        t = buffer.cur(2)?.glyph_id;
                        if is_t(t) {
                            // Only used if isCombiningT (t); otherwise invalid.
                            tindex = t - T_BASE;
                        } else {
                            // The next character was not a trailing jamo.
                            t = 0;
                        }
                    }

                    let offset = if t != 0 { 3 } else { 2 };
                    buffer.unsafe_to_break(Some(buffer.idx), Some(buffer.idx + offset));

                    // We've got a syllable <L,V,T?>; see if it can potentially be composed.
                    if is_combining_l(l) && is_combining_v(v) && (t == 0 || is_combining_t(t)) {
                        // Try to compose; if this succeeds, end is set to start+1.
                        let s = S_BASE + (l - L_BASE) * N_COUNT + (v - V_BASE) * T_COUNT + tindex;
                        if font.has_glyph(s) {
                            let n = if t != 0 { 3 } else { 2 };
                            buffer.replace_glyphs(n, 1, &[s]);
                            end = start + 1;
                            continue;
                        }
                    }

                    // We didn't compose, either because it's an Old Hangul syllable without a
                    // precomposed character in Unicode, or because the font didn't support the
                    // necessary precomposed glyph.
                    // Set jamo features on the individual glyphs, and advance past them.
                    buffer.cur_mut(0)?.set_hangul_shaping_feature(LJMO);
                    buffer.next_glyph();
                    buffer.cur_mut(0)?.set_hangul_shaping_feature(VJMO);
                    buffer.next_glyph();
                    if t != 0 {
                        buffer.cur_mut(0)?.set_hangul_shaping_feature(TJMO);
                        buffer.next_glyph();
                        end = start + 3;
                    } else {
                        end = start + 2;
                    }

                    buffer.merge_out_grapheme_clusters(start, end);

                    continue;
                }
            } else if is_combined_s(u) {
                // Have <LV>, <LVT>, or <LV,T>
                let s = u;
                let has_glyph = font.has_glyph(s);

                let lindex = (s - S_BASE) / N_COUNT;
                let nindex = (s - S_BASE) % N_COUNT;
                let vindex = nindex / T_COUNT;
                let tindex = nindex % T_COUNT;

                if tindex == 0
                    && buffer.idx + 1 < buffer.len
                    && is_combining_t(buffer.cur(1)?.glyph_id)
                {
                    // <LV,T>, try to combine.
                    let new_tindex = buffer.cur(1)?.glyph_id - T_BASE;
                    let new_s = s + new_tindex;

                    if font.has_glyph(new_s) {
                        buffer.replace_glyphs(2, 1, &[new_s]);
                        end = start + 1;
                        continue;
                    } else {
                        // Mark unsafe between LV and T.
                        buffer.unsafe_to_break(Some(buffer.idx), Some(buffer.idx + 2));
                    }
                }

                // Otherwise, decompose if font doesn't support <LV> or <LVT>,
                // or if having non-combining <LV,T>.  Note that we already handled
                // combining <LV,T> above.
                if !has_glyph
                    || (tindex == 0 && buffer.idx + 1 < buffer.len && is_t(buffer.cur(1)?.glyph_id))
                {
                    let decomposed = [L_BASE + lindex, V_BASE + vindex, T_BASE + tindex];
                    if font.has_glyph(decomposed[0])
                        && font.has_glyph(decomposed[1])
                        && (tindex == 0 || font.has_glyph(decomposed[2]))
                    {
                        let mut s_len = if tindex != 0 { 3 } else { 2 };
                        buffer.replace_glyphs(1, s_len, &decomposed);

                        // If we decomposed an LV because of a non-combining T following,
                        // we want to include this T in the syllable.
                        if has_glyph && tindex == 0 {
                            buffer.next_glyph();
                            s_len += 1;
                        }

                        // We decomposed S: apply jamo features to the individual glyphs
                        // that are now in `buffer.out_info`.
                        end = start + s_len;

                        let jamos = buffer
                            .out_info_mut()
                            .get_mut(start..end)
                            .unwrap_or_default();
                        for (info, feature) in jamos.iter_mut().zip([LJMO, VJMO, TJMO]) {
                            info.set_hangul_shaping_feature(feature);
                        }

                        buffer.merge_out_grapheme_clusters(start, end);

                        continue;
                    } else if tindex == 0
                        && buffer.idx + 1 > buffer.len
                        && is_t(buffer.cur(1)?.glyph_id)
                    {
                        // Mark unsafe between LV and T.
                        buffer.unsafe_to_break(Some(buffer.idx), Some(buffer.idx + 2));
                    }
                }

                if has_glyph {
                    // We didn't decompose the S, so just advance past it.
                    end = start + 1;
                    buffer.next_glyph();
                    continue;
                }
            }

            // Didn't find a recognizable syllable, so we leave end <= start;
            // this will prevent tone-mark reordering happening.
            buffer.next_glyph();
        }

        buffer.sync();
        Some(())
    }

    fn setup_masks_hangul(plan: &ShapePlan, _: &ShaperFont<'_, '_>, buffer: &mut Buffer) {
        let hangul_plan = plan.data::<HangulShapePlan>();
        for info in buffer.info_slice_mut() {
            info.mask |= hangul_plan.mask_array[info.hangul_shaping_feature() as usize];

            // Keep 'calt' away from jamo; see override_features_hangul().
            let u = info.glyph_id;
            if is_l(u) || is_v(u) || is_t(u) {
                info.mask &= !hangul_plan.calt_mask;
            }
        }

        buffer.deallocate_var(GlyphInfo::HANGUL_SHAPING_FEATURE_VAR);
    }

    pub const HANGUL_SHAPER: OtShaper = OtShaper {
        collect_features: Some(collect_features_hangul),
        override_features: Some(override_features_hangul),
        create_data: Some(|plan| Box::new(data_create_hangul(&plan.ot_map))),
        preprocess_text: Some(preprocess_text_hangul),
        postprocess_glyphs: None,
        normalization_preference: NormalizationMode::None,
        decompose: None,
        compose: None,
        setup_masks: Some(setup_masks_hangul),
        gpos_tag: None,
        reorder_marks: None,
        zero_width_marks: ZeroWidthMarks::None,
        fallback_position: true,
    };
}

pub(crate) mod hebrew {
    use crate::buffer::Buffer;
    use crate::ot::shaper::*;
    use crate::plan::ShapePlan;
    use crate::unicode::{self, combining_class, modified_combining_class, Codepoint};
    use crate::Tag;

    pub const HEBREW_SHAPER: OtShaper = OtShaper {
        collect_features: None,
        override_features: None,
        create_data: None,
        preprocess_text: None,
        postprocess_glyphs: None,
        normalization_preference: NormalizationMode::Auto,
        decompose: None,
        compose: Some(compose),
        setup_masks: None,
        gpos_tag: Some(Tag::new(b"hebr")),
        reorder_marks: Some(reorder_marks_hebrew),
        zero_width_marks: ZeroWidthMarks::ByGdefLate,
        fallback_position: true,
    };

    fn reorder_marks_hebrew(_: &ShapePlan, buffer: &mut Buffer, start: usize, end: usize) {
        let marks = buffer.info.get(start..end).unwrap_or_default();
        let found = marks.windows(3).position(|window| {
            let [c0, c1, c2] = window else {
                return false;
            };

            // patach or qamats
            (c0.modified_combining_class() == modified_combining_class::CCC17
                || c0.modified_combining_class() == modified_combining_class::CCC18)
                // sheva or hiriq
                && (c1.modified_combining_class() == modified_combining_class::CCC10
                    || c1.modified_combining_class() == modified_combining_class::CCC14)
                // meteg or below
                && (c2.modified_combining_class() == modified_combining_class::CCC22
                    || c2.modified_combining_class() == combining_class::Below)
        });

        if let Some(offset) = found {
            let i = start + offset + 2;
            buffer.merge_clusters(i - 1, i + 1);
            if let Some(pair) = buffer.info.get_mut(i - 1..=i) {
                pair.swap(0, 1);
            }
        }
    }

    static S_DAGESH_FORMS: &[Codepoint] = &[
        0xFB30, // ALEF
        0xFB31, // BET
        0xFB32, // GIMEL
        0xFB33, // DALET
        0xFB34, // HE
        0xFB35, // VAV
        0xFB36, // ZAYIN
        0x0000, // HET
        0xFB38, // TET
        0xFB39, // YOD
        0xFB3A, // FINAL KAF
        0xFB3B, // KAF
        0xFB3C, // LAMED
        0x0000, // FINAL MEM
        0xFB3E, // MEM
        0x0000, // FINAL NUN
        0xFB40, // NUN
        0xFB41, // SAMEKH
        0x0000, // AYIN
        0xFB43, // FINAL PE
        0xFB44, // PE
        0x0000, // FINAL TSADI
        0xFB46, // TSADI
        0xFB47, // QOF
        0xFB48, // RESH
        0xFB49, // SHIN
        0xFB4A, // TAV
    ];

    fn compose(ctx: &NormalizeContext, a: Codepoint, b: Codepoint) -> Option<Codepoint> {
        // Hebrew presentation-form shaping.
        // https://bugzilla.mozilla.org/show_bug.cgi?id=728866
        // Hebrew presentation forms with dagesh, for characters U+05D0..05EA;
        // Note that some letters do not have a dagesh presForm encoded.
        match unicode::compose(a, b) {
            Some(c) => Some(c),
            None if !ctx.plan.has_gpos_mark => {
                // Special-case Hebrew presentation forms that are excluded from
                // standard normalization, but wanted for old fonts.
                match b {
                    0x05B4 => {
                        // HIRIQ
                        match a {
                            0x05D9 => Some(0xFB1D), // YOD
                            _ => None,
                        }
                    }
                    0x05B7 => {
                        // PATAH
                        match a {
                            0x05D9 => Some(0xFB1F), // YIDDISH YOD YOD
                            0x05D0 => Some(0xFB2E), // ALEF
                            _ => None,
                        }
                    }
                    0x05B8 => {
                        // QAMATS
                        match a {
                            0x05D0 => Some(0xFB2F), // ALEF
                            _ => None,
                        }
                    }
                    0x05B9 => {
                        // HOLAM
                        match a {
                            0x05D5 => Some(0xFB4B), // VAV
                            _ => None,
                        }
                    }
                    0x05BC => {
                        // DAGESH
                        match a {
                            0x05D0..=0x05EA => {
                                let c = S_DAGESH_FORMS[a as usize - 0x05D0];
                                if c != 0 {
                                    Some(c)
                                } else {
                                    None
                                }
                            }
                            0xFB2A => Some(0xFB2C), // SHIN WITH SHIN DOT
                            0xFB2B => Some(0xFB2D), // SHIN WITH SIN DOT
                            _ => None,
                        }
                    }
                    0x05BF => {
                        // RAFE
                        match a {
                            0x05D1 => Some(0xFB4C), // BET
                            0x05DB => Some(0xFB4D), // KAF
                            0x05E4 => Some(0xFB4E), // PE
                            _ => None,
                        }
                    }
                    0x05C1 => {
                        // SHIN DOT
                        match a {
                            0x05E9 => Some(0xFB2A), // SHIN
                            0xFB49 => Some(0xFB2C), // SHIN WITH DAGESH
                            _ => None,
                        }
                    }
                    0x05C2 => {
                        // SIN DOT
                        match a {
                            0x05E9 => Some(0xFB2B), // SHIN
                            0xFB49 => Some(0xFB2D), // SHIN WITH DAGESH
                            _ => None,
                        }
                    }
                    _ => None,
                }
            }
            None => None,
        }
    }
}

pub(crate) mod thai {
    use crate::normalize::NormalizationMode;
    use crate::ot::layout::*;
    use crate::ot::shaper::*;
    use crate::plan::ShapePlan;
    use crate::unicode::GeneralCategory;
    use crate::Script;
    use crate::ShaperFont;

    pub const THAI_SHAPER: OtShaper = OtShaper {
        collect_features: None,
        override_features: None,
        create_data: None,
        preprocess_text: Some(preprocess_text),
        postprocess_glyphs: None,
        normalization_preference: NormalizationMode::Auto,
        decompose: None,
        compose: None,
        setup_masks: None,
        gpos_tag: None,
        reorder_marks: None,
        zero_width_marks: ZeroWidthMarks::ByGdefLate,
        fallback_position: false,
    };

    #[derive(Clone, Copy, PartialEq)]
    enum Consonant {
        NC = 0,
        AC,
        RC,
        DC,
        NotConsonant,
    }

    fn get_consonant_type(u: u32) -> Consonant {
        match u {
            0x0E1B | 0x0E1D | 0x0E1F => Consonant::AC,
            0x0E0D | 0x0E10 => Consonant::RC,
            0x0E0E | 0x0E0F => Consonant::DC,
            0x0E01..=0x0E2E => Consonant::NC,
            _ => Consonant::NotConsonant,
        }
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        AV,
        BV,
        T,
        NotMark,
    }

    fn get_mark_type(u: u32) -> Mark {
        match u {
            0x0E31 | 0x0E34..=0x0E37 | 0x0E47 | 0x0E4D..=0x0E4E => Mark::AV,
            0x0E38..=0x0E3A => Mark::BV,
            0x0E48..=0x0E4C => Mark::T,
            _ => Mark::NotMark,
        }
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Action {
        NOP,
        /// Shift combining-mark down.
        SD,
        /// Shift combining-mark left.
        SL,
        /// Shift combining-mark down-left.
        SDL,
        /// Remove descender from base.
        RD,
    }

    #[derive(Clone, Copy)]
    struct PuaMapping {
        u: u16,
        win_pua: u16,
        mac_pua: u16,
    }

    impl PuaMapping {
        const fn new(u: u16, win_pua: u16, mac_pua: u16) -> Self {
            PuaMapping {
                u,
                win_pua,
                mac_pua,
            }
        }
    }

    static SD_MAPPINGS: &[PuaMapping] = &[
        PuaMapping::new(0x0E48, 0xF70A, 0xF88B), // MAI EK
        PuaMapping::new(0x0E49, 0xF70B, 0xF88E), // MAI THO
        PuaMapping::new(0x0E4A, 0xF70C, 0xF891), // MAI TRI
        PuaMapping::new(0x0E4B, 0xF70D, 0xF894), // MAI CHATTAWA
        PuaMapping::new(0x0E4C, 0xF70E, 0xF897), // THANTHAKHAT
        PuaMapping::new(0x0E38, 0xF718, 0xF89B), // SARA U
        PuaMapping::new(0x0E39, 0xF719, 0xF89C), // SARA UU
        PuaMapping::new(0x0E3A, 0xF71A, 0xF89D), // PHINTHU
        PuaMapping::new(0x0000, 0x0000, 0x0000),
    ];

    static SDL_MAPPINGS: &[PuaMapping] = &[
        PuaMapping::new(0x0E48, 0xF705, 0xF88C), // MAI EK
        PuaMapping::new(0x0E49, 0xF706, 0xF88F), // MAI THO
        PuaMapping::new(0x0E4A, 0xF707, 0xF892), // MAI TRI
        PuaMapping::new(0x0E4B, 0xF708, 0xF895), // MAI CHATTAWA
        PuaMapping::new(0x0E4C, 0xF709, 0xF898), // THANTHAKHAT
        PuaMapping::new(0x0000, 0x0000, 0x0000),
    ];

    static SL_MAPPINGS: &[PuaMapping] = &[
        PuaMapping::new(0x0E48, 0xF713, 0xF88A), // MAI EK
        PuaMapping::new(0x0E49, 0xF714, 0xF88D), // MAI THO
        PuaMapping::new(0x0E4A, 0xF715, 0xF890), // MAI TRI
        PuaMapping::new(0x0E4B, 0xF716, 0xF893), // MAI CHATTAWA
        PuaMapping::new(0x0E4C, 0xF717, 0xF896), // THANTHAKHAT
        PuaMapping::new(0x0E31, 0xF710, 0xF884), // MAI HAN-AKAT
        PuaMapping::new(0x0E34, 0xF701, 0xF885), // SARA I
        PuaMapping::new(0x0E35, 0xF702, 0xF886), // SARA II
        PuaMapping::new(0x0E36, 0xF703, 0xF887), // SARA UE
        PuaMapping::new(0x0E37, 0xF704, 0xF888), // SARA UEE
        PuaMapping::new(0x0E47, 0xF712, 0xF889), // MAITAIKHU
        PuaMapping::new(0x0E4D, 0xF711, 0xF899), // NIKHAHIT
        PuaMapping::new(0x0000, 0x0000, 0x0000),
    ];

    static RD_MAPPINGS: &[PuaMapping] = &[
        PuaMapping::new(0x0E0D, 0xF70F, 0xF89A), // YO YING
        PuaMapping::new(0x0E10, 0xF700, 0xF89E), // THO THAN
        PuaMapping::new(0x0000, 0x0000, 0x0000),
    ];

    fn pua_shape(u: u32, action: Action, font: &ShaperFont<'_, '_>) -> u32 {
        let mappings = match action {
            Action::NOP => return u,
            Action::SD => SD_MAPPINGS,
            Action::SL => SL_MAPPINGS,
            Action::SDL => SDL_MAPPINGS,
            Action::RD => RD_MAPPINGS,
        };

        for m in mappings {
            if m.u as u32 == u {
                if font.nominal_glyph(m.win_pua as u32).is_some() {
                    return m.win_pua as u32;
                }

                if font.nominal_glyph(m.mac_pua as u32).is_some() {
                    return m.mac_pua as u32;
                }

                break;
            }
        }

        u
    }

    #[derive(Clone, Copy)]
    enum AboveState {
        // Cluster above looks like:
        T0, //  ⣤
        T1, //     ⣼
        T2, //        ⣾
        T3, //           ⣿
    }

    static ABOVE_START_STATE: &[AboveState] = &[
        AboveState::T0, // NC
        AboveState::T1, // AC
        AboveState::T0, // RC
        AboveState::T0, // DC
        AboveState::T3, // NotConsonant
    ];

    #[derive(Clone, Copy)]
    struct AboveStateMachineEdge {
        action: Action,
        next_state: AboveState,
    }

    impl AboveStateMachineEdge {
        const fn new(action: Action, next_state: AboveState) -> Self {
            AboveStateMachineEdge { action, next_state }
        }
    }

    type ASME = AboveStateMachineEdge;

    static ABOVE_STATE_MACHINE: &[[ASME; 3]] = &[
        //        AV                                      BV                                      T
        /* T0 */
        [
            ASME::new(Action::NOP, AboveState::T3),
            ASME::new(Action::NOP, AboveState::T0),
            ASME::new(Action::SD, AboveState::T3),
        ],
        /* T1 */
        [
            ASME::new(Action::SL, AboveState::T2),
            ASME::new(Action::NOP, AboveState::T1),
            ASME::new(Action::SDL, AboveState::T2),
        ],
        /* T2 */
        [
            ASME::new(Action::NOP, AboveState::T3),
            ASME::new(Action::NOP, AboveState::T2),
            ASME::new(Action::SL, AboveState::T3),
        ],
        /* T3 */
        [
            ASME::new(Action::NOP, AboveState::T3),
            ASME::new(Action::NOP, AboveState::T3),
            ASME::new(Action::NOP, AboveState::T3),
        ],
    ];

    #[derive(Clone, Copy)]
    enum BelowState {
        /// No descender.
        B0,
        /// Removable descender.
        B1,
        /// Strict descender.
        B2,
    }

    static BELOW_START_STATE: &[BelowState] = &[
        BelowState::B0, // NC
        BelowState::B0, // AC
        BelowState::B1, // RC
        BelowState::B2, // DC
        BelowState::B2, // NotConsonant
    ];

    #[derive(Clone, Copy)]
    struct BelowStateMachineEdge {
        action: Action,
        next_state: BelowState,
    }

    impl BelowStateMachineEdge {
        const fn new(action: Action, next_state: BelowState) -> Self {
            BelowStateMachineEdge { action, next_state }
        }
    }

    type BSME = BelowStateMachineEdge;

    static BELOW_STATE_MACHINE: &[[BSME; 3]] = &[
        //        AV                                      BV                                      T
        /* B0 */
        [
            BSME::new(Action::NOP, BelowState::B0),
            BSME::new(Action::NOP, BelowState::B2),
            BSME::new(Action::NOP, BelowState::B0),
        ],
        /* B1 */
        [
            BSME::new(Action::NOP, BelowState::B1),
            BSME::new(Action::RD, BelowState::B2),
            BSME::new(Action::NOP, BelowState::B1),
        ],
        /* B2 */
        [
            BSME::new(Action::NOP, BelowState::B2),
            BSME::new(Action::SD, BelowState::B2),
            BSME::new(Action::NOP, BelowState::B2),
        ],
    ];

    fn do_pua_shaping(font: &ShaperFont<'_, '_>, buffer: &mut Buffer) -> Option<()> {
        let mut above_state = ABOVE_START_STATE[Consonant::NotConsonant as usize];
        let mut below_state = BELOW_START_STATE[Consonant::NotConsonant as usize];
        let mut base = 0;

        for i in 0..buffer.len {
            let glyph = buffer.info.get(i)?.glyph_id;
            let mt = get_mark_type(glyph);

            if mt == Mark::NotMark {
                let ct = get_consonant_type(glyph);
                above_state = ABOVE_START_STATE[ct as usize];
                below_state = BELOW_START_STATE[ct as usize];
                base = i;
                continue;
            }

            let above_edge = ABOVE_STATE_MACHINE[above_state as usize][mt as usize];
            let below_edge = BELOW_STATE_MACHINE[below_state as usize][mt as usize];
            above_state = above_edge.next_state;
            below_state = below_edge.next_state;

            // At least one of the above/below actions is NOP.
            let action = if above_edge.action != Action::NOP {
                above_edge.action
            } else {
                below_edge.action
            };

            buffer.unsafe_to_break(Some(base), Some(i));
            let target = if action == Action::RD { base } else { i };
            let info = buffer.info.get_mut(target)?;
            info.glyph_id = pua_shape(info.glyph_id, action, font);
        }
        Some(())
    }

    /// Moves Nikhahit, just output at `end - 2`, ahead of the above-base marks
    /// before it, and returns where it lands.
    fn move_nikhahit(buffer: &mut Buffer, end: usize) -> Option<usize> {
        #[inline]
        fn is_above_base_mark(u: u32) -> bool {
            let u = u & !0x0080;
            matches!(u, 0x0E34..=0x0E37 | 0x0E47..=0x0E4E | 0x0E31..=0x0E31 | 0x0E3B..=0x0E3B)
        }

        let nikhahit = end.checked_sub(2)?;
        // Make Nikhahit be recognized as a ccc=0 mark when zeroing widths.
        buffer
            .out_info_mut()
            .get_mut(nikhahit)?
            .set_general_category(GeneralCategory::NON_SPACING_MARK);

        let mut start = nikhahit;
        while start > 0 && is_above_base_mark(buffer.out_info().get(start - 1)?.glyph_id) {
            start -= 1;
        }
        if start < nikhahit {
            buffer.merge_out_clusters(start, end);
            buffer
                .out_info_mut()
                .get_mut(start..=nikhahit)?
                .rotate_right(1);
        }
        Some(start)
    }

    // TODO: more tests
    fn preprocess_text(
        plan: &ShapePlan,
        font: &ShaperFont<'_, '_>,
        buffer: &mut Buffer,
    ) -> Option<()> {
        // This function implements the shaping logic documented here:
        //
        //   https://linux.thai.net/~thep/th-otf/shaping.html
        //
        // The first shaping rule listed there is needed even if the font has Thai
        // OpenType tables.  The rest do fallback positioning based on PUA codepoints.
        // We implement that only if there exist no Thai GSUB in the font.

        // The following is NOT specified in the MS OT Thai spec, however, it seems
        // to be what Uniscribe and other engines implement.  According to Eric Muller:
        //
        // When you have a SARA AM, decompose it in NIKHAHIT + SARA AA, *and* move the
        // NIKHAHIT backwards over any above-base marks (0E31, 0E34-0E37, 0E47-0E4E).
        //
        // <0E14, 0E4B, 0E33> -> <0E14, 0E4D, 0E4B, 0E32>
        //
        // This reordering is legit only when the NIKHAHIT comes from a SARA AM, not
        // when it's there to start with. The string <0E14, 0E4B, 0E4D> is probably
        // not what a user wanted, but the rendering is nevertheless nikhahit above
        // chattawa.
        //
        // Same for Lao.
        //
        // Note:
        //
        // Uniscribe also does some below-marks reordering.  Namely, it positions U+0E3A
        // after U+0E38 and U+0E39.  We do that by modifying the ccc for U+0E3A.
        // See unicode->modified_combining_class ().  Lao does NOT have a U+0E3A
        // equivalent.

        // Here are the characters of significance:
        //
        //              Thai    Lao
        // SARA AM:     U+0E33  U+0EB3
        // SARA AA:     U+0E32  U+0EB2
        // Nikhahit:    U+0E4D  U+0ECD
        //
        // Testing shows that Uniscribe reorder the following marks:
        // Thai:	<0E31,0E34..0E37,0E47..0E4E>
        // Lao:     <0EB1,0EB4..0EB7,0EBB,0EC8..0ECD>
        //
        // Note how the Lao versions are the same as Thai + 0x80.

        // We only get one script at a time, so a script-agnostic implementation
        // is adequate here.
        #[inline]
        fn is_sara_am(u: u32) -> bool {
            (u & !0x0080) == 0x0E33
        }
        #[inline]
        fn nikhahit_from_sara_am(u: u32) -> u32 {
            u - 0x0E33 + 0x0E4D
        }
        #[inline]
        fn sara_aa_from_sara_am(u: u32) -> u32 {
            u - 1
        }
        buffer.clear_output();
        buffer.idx = 0;
        while buffer.idx < buffer.len {
            let u = buffer.cur(0)?.glyph_id;
            if !is_sara_am(u) {
                buffer.next_glyph();
                continue;
            }

            // Is SARA AM. Decompose and reorder.
            buffer.output_glyph(nikhahit_from_sara_am(u));
            if let Some(&info) = buffer.out_info().get(buffer.out_len - 1) {
                let mut info = info;
                info.set_continuation(&mut buffer.scratch_flags);
            }
            buffer.replace_glyph(sara_aa_from_sara_am(u));

            let end = buffer.out_len;
            let start = move_nikhahit(buffer, end).unwrap_or(end - 2);

            // Since we decomposed, and NIKHAHIT is combining, merge clusters with the
            // previous cluster.
            if start != 0 {
                buffer.merge_out_grapheme_clusters(start - 1, end);
            }
        }

        buffer.sync();

        // If font has Thai GSUB, we are done.
        if plan.script == Some(Script::THAI) && !plan.ot_map.found_script(LayoutTableKind::Gsub) {
            if do_pua_shaping(font, buffer).is_none() {
                buffer.index_failed();
            }
        }
        Some(())
    }
}
