pub(crate) mod direction {
    use core::str::FromStr;

    use crate::Script;

    /// Defines the direction in which text is to be read.
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    pub enum Direction {
        /// Initial, unset direction.
        Invalid,
        /// Text is set horizontally from left to right.
        LeftToRight,
        /// Text is set horizontally from right to left.
        RightToLeft,
        /// Text is set vertically from top to bottom.
        TopToBottom,
        /// Text is set vertically from bottom to top.
        BottomToTop,
    }

    impl Direction {
        #[inline]
        pub(crate) fn is_horizontal(self) -> bool {
            match self {
                Direction::Invalid => false,
                Direction::LeftToRight => true,
                Direction::RightToLeft => true,
                Direction::TopToBottom => false,
                Direction::BottomToTop => false,
            }
        }

        #[inline]
        pub(crate) fn is_vertical(self) -> bool {
            !self.is_horizontal()
        }

        #[inline]
        pub(crate) fn is_forward(self) -> bool {
            match self {
                Direction::Invalid => false,
                Direction::LeftToRight => true,
                Direction::RightToLeft => false,
                Direction::TopToBottom => true,
                Direction::BottomToTop => false,
            }
        }

        #[inline]
        pub(crate) fn is_backward(self) -> bool {
            !self.is_forward()
        }

        #[inline]
        pub(crate) fn reverse(self) -> Self {
            match self {
                Direction::Invalid => Direction::Invalid,
                Direction::LeftToRight => Direction::RightToLeft,
                Direction::RightToLeft => Direction::LeftToRight,
                Direction::TopToBottom => Direction::BottomToTop,
                Direction::BottomToTop => Direction::TopToBottom,
            }
        }

        pub(crate) fn from_script(script: Script) -> Option<Self> {
            // https://docs.google.com/spreadsheets/d/1Y90M0Ie3MUJ6UVCRDOypOtijlMDLNNyyLk36T6iMu0o

            match script {
                // Unicode-1.1 additions
                Script::ARABIC |
                Script::HEBREW |

                // Unicode-3.0 additions
                Script::SYRIAC |
                Script::THAANA |

                // Unicode-4.0 additions
                Script::CYPRIOT |

                // Unicode-4.1 additions
                Script::KHAROSHTHI |

                // Unicode-5.0 additions
                Script::PHOENICIAN |
                Script::NKO |

                // Unicode-5.1 additions
                Script::LYDIAN |

                // Unicode-5.2 additions
                Script::AVESTAN |
                Script::IMPERIAL_ARAMAIC |
                Script::INSCRIPTIONAL_PAHLAVI |
                Script::INSCRIPTIONAL_PARTHIAN |
                Script::OLD_SOUTH_ARABIAN |
                Script::OLD_TURKIC |
                Script::SAMARITAN |

                // Unicode-6.0 additions
                Script::MANDAIC |

                // Unicode-6.1 additions
                Script::MEROITIC_CURSIVE |
                Script::MEROITIC_HIEROGLYPHS |

                // Unicode-7.0 additions
                Script::MANICHAEAN |
                Script::MENDE_KIKAKUI |
                Script::NABATAEAN |
                Script::OLD_NORTH_ARABIAN |
                Script::PALMYRENE |
                Script::PSALTER_PAHLAVI |

                // Unicode-8.0 additions
                Script::HATRAN |

                // Unicode-9.0 additions
                Script::ADLAM |

                // Unicode-11.0 additions
                Script::HANIFI_ROHINGYA |
                Script::OLD_SOGDIAN |
                Script::SOGDIAN |

                // Unicode-12.0 additions
                Script::ELYMAIC |

                // Unicode-13.0 additions
                Script::CHORASMIAN |
                Script::YEZIDI |

                // Unicode-14.0 additions
                Script::OLD_UYGHUR |

                // Unicode-16.0 additions
                Script::GARAY |

                // Unicode-17.0 additions
                Script::SIDETIC => {
                    Some(Direction::RightToLeft)
                }

                // https://github.com/harfbuzz/harfbuzz/issues/1000
                Script::OLD_HUNGARIAN |
                Script::OLD_ITALIC |
                Script::RUNIC |
                Script::TIFINAGH => {
                    None
                }

                _ => Some(Direction::LeftToRight),
            }
        }
    }

    impl Default for Direction {
        #[inline]
        fn default() -> Self {
            Direction::Invalid
        }
    }

    impl FromStr for Direction {
        type Err = &'static str;

        fn from_str(s: &str) -> Result<Self, Self::Err> {
            if s.is_empty() {
                return Err("invalid direction");
            }

            // harfbuzz also matches only the first letter.
            match s.as_bytes()[0].to_ascii_lowercase() {
                b'l' => Ok(Direction::LeftToRight),
                b'r' => Ok(Direction::RightToLeft),
                b't' => Ok(Direction::TopToBottom),
                b'b' => Ok(Direction::BottomToTop),
                _ => Err("invalid direction"),
            }
        }
    }
}

pub(crate) mod error {
    use crate::{Direction, Script};

    /// The reason a call to [`shape`](crate::shape) could not produce glyphs.
    ///
    /// Each of these is a misuse of the API. Running out of room is not among
    /// them: pathological input can provoke it, so it is reported through
    /// [`Buffer::allocation_successful`](crate::Buffer::allocation_successful)
    /// rather than as a failure to shape.
    #[derive(Clone, PartialEq, Eq, Hash, Debug)]
    #[non_exhaustive]
    pub enum ShapeError {
        /// The buffer already holds the glyphs from an earlier call.
        ///
        /// To shape the same contents again, set the content type back to
        /// [`ContentType::Unicode`](crate::ContentType::Unicode) with
        /// [`set_content_type`](crate::Buffer::set_content_type); to shape something else,
        /// clear the buffer and fill it afresh.
        AlreadyShaped,

        /// The buffer has no direction, so no plan could be built for it.
        ///
        /// Call [`guess_segment_properties`](crate::Buffer::guess_segment_properties) to
        /// infer one from the contents, or set it with
        /// [`set_direction`](crate::Buffer::set_direction).
        DirectionUnset,

        /// The buffer's direction is not the one the supplied plan was built for.
        DirectionMismatch {
            /// The direction the plan was built for.
            plan: Direction,
            /// The direction the buffer carries.
            buffer: Direction,
        },

        /// The buffer's script is not the one the supplied plan was built for.
        ScriptMismatch {
            /// The script the plan was built for.
            plan: Script,
            /// The script the buffer carries.
            buffer: Script,
        },
    }

    impl core::fmt::Display for ShapeError {
        fn fmt(&self, fmt: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::AlreadyShaped => fmt.write_str("buffer already holds shaped glyphs"),
                Self::DirectionUnset => fmt.write_str("buffer has no direction set"),
                Self::DirectionMismatch { plan, buffer } => write!(
                    fmt,
                    "buffer direction does not match plan direction: {buffer:?} != {plan:?}"
                ),
                Self::ScriptMismatch { plan, buffer } => write!(
                    fmt,
                    "buffer script does not match plan script: {buffer:?} != {plan:?}"
                ),
            }
        }
    }

    impl std::error::Error for ShapeError {}
}

pub(crate) mod feature {
    use crate::{text_parser::TextParser, Tag};
    use core::{
        ops::{Bound, RangeBounds},
        str::FromStr,
    };

    /// An OpenType feature and the text range where it applies.
    #[repr(C)]
    #[derive(Clone, Copy, PartialEq, Hash, Debug)]
    pub struct Feature {
        /// OpenType feature tag.
        pub tag: Tag,
        /// Value assigned to the feature, usually `0` to disable or `1` to enable it.
        pub value: u32,
        /// First cluster index to which the feature applies.
        pub start: u32,
        /// Upper cluster index of the feature's range.
        pub end: u32,
    }

    impl Feature {
        /// Start index for a feature that applies to the whole buffer.
        pub const GLOBAL_START: u32 = 0;
        /// End index for a feature that applies to the whole buffer.
        pub const GLOBAL_END: u32 = u32::MAX;

        /// Creates a feature for the given tag, value, and cluster range.
        pub fn new(tag: Tag, value: u32, range: impl RangeBounds<usize>) -> Feature {
            let max = Self::GLOBAL_END as usize;
            let start = match range.start_bound() {
                Bound::Included(&included) => included.min(max) as u32,
                Bound::Excluded(&excluded) => excluded.min(max - 1) as u32 + 1,
                Bound::Unbounded => Self::GLOBAL_START,
            };
            let end = match range.end_bound() {
                Bound::Included(&included) => included.min(max) as u32,
                Bound::Excluded(&excluded) => excluded.saturating_sub(1).min(max) as u32,
                Bound::Unbounded => Self::GLOBAL_END,
            };

            Feature {
                tag,
                value,
                start,
                end,
            }
        }

        pub(crate) fn is_global(&self) -> bool {
            self.start == Self::GLOBAL_START && self.end == Self::GLOBAL_END
        }
    }

    impl FromStr for Feature {
        type Err = &'static str;

        /// Parses HarfBuzz feature syntax.
        ///
        /// Possible values:
        ///
        /// - `kern` and `+kern` enable kerning globally.
        /// - `-kern` and `kern=0` disable it globally.
        /// - `aalt=2` selects value 2 for the `aalt` feature.
        /// - `kern[5:]` applies from cluster 5 onward.
        /// - `kern[3:5]` applies to clusters 3 and 4.
        /// - `kern[3]` applies to cluster 3.
        fn from_str(s: &str) -> Result<Self, Self::Err> {
            fn parse(s: &str) -> Option<Feature> {
                if s.is_empty() {
                    return None;
                }

                let mut p = TextParser::new(s);

                // Parse prefix.
                let mut value = 1;
                match p.curr_byte()? {
                    b'-' => {
                        value = 0;
                        p.advance(1);
                    }
                    b'+' => {
                        value = 1;
                        p.advance(1);
                    }
                    _ => {}
                }

                // Parse tag.
                p.skip_spaces();
                let quote = p.consume_quote();

                let tag = p.consume_tag()?;

                // Force closing quote.
                if let Some(quote) = quote {
                    p.consume_byte(quote)?;
                }

                // Parse indices.
                p.skip_spaces();

                let (start, end) = if p.consume_byte(b'[').is_some() {
                    let start_opt = p.consume_i32();
                    let start = start_opt.unwrap_or(0) as u32; // negative value overflow is ok

                    let end = if matches!(p.curr_byte(), Some(b':' | b';')) {
                        p.advance(1);
                        p.consume_i32().unwrap_or(-1) as u32 // negative value overflow is ok
                    } else {
                        if start_opt.is_some() && start != Feature::GLOBAL_END {
                            start + 1
                        } else {
                            Feature::GLOBAL_END
                        }
                    };

                    p.consume_byte(b']')?;

                    (start, end)
                } else {
                    (Feature::GLOBAL_START, Feature::GLOBAL_END)
                };

                // Parse postfix.
                let had_equal = p.consume_byte(b'=').is_some();
                let value1 = p
                    .consume_i32()
                    .or_else(|| p.consume_bool().map(|b| b as i32));

                if had_equal && value1.is_none() {
                    return None;
                }

                if let Some(value1) = value1 {
                    value = value1 as u32; // negative value overflow is ok
                }

                p.skip_spaces();

                if !p.at_end() {
                    return None;
                }

                Some(Feature {
                    tag,
                    value,
                    start,
                    end,
                })
            }

            parse(s).ok_or("invalid feature")
        }
    }

    #[cfg(test)]
    mod tests_features {
        use super::*;
        use core::str::FromStr;

        macro_rules! test {
            ($name:ident, $text:expr, $tag:expr, $value:expr, $range:expr) => {
                #[test]
                fn $name() {
                    assert_eq!(
                        Feature::from_str($text).unwrap(),
                        Feature::new(Tag::new($tag), $value, $range)
                    );
                }
            };
        }

        test!(parse_01, "kern", b"kern", 1, ..);
        test!(parse_02, "+kern", b"kern", 1, ..);
        test!(parse_03, "-kern", b"kern", 0, ..);
        test!(parse_04, "kern=0", b"kern", 0, ..);
        test!(parse_05, "kern=1", b"kern", 1, ..);
        test!(parse_06, "kern=2", b"kern", 2, ..);
        test!(parse_07, "kern[]", b"kern", 1, ..);
        test!(parse_08, "kern[:]", b"kern", 1, ..);
        test!(parse_09, "kern[5:]", b"kern", 1, 5..);
        test!(parse_10, "kern[:5]", b"kern", 1, ..=5);
        test!(parse_11, "kern[3:5]", b"kern", 1, 3..=5);
        test!(parse_12, "kern[3]", b"kern", 1, 3..=4);
        test!(parse_13, "kern[3:5]=2", b"kern", 2, 3..=5);
        test!(parse_14, "kern[3;5]=2", b"kern", 2, 3..=5);
        test!(parse_15, "kern[:-1]", b"kern", 1, ..);
        test!(parse_16, "kern[-1]", b"kern", 1, u32::MAX as usize..);
        test!(parse_17, "kern=on", b"kern", 1, ..);
        test!(parse_18, "kern=off", b"kern", 0, ..);
        test!(parse_19, "kern=oN", b"kern", 1, ..);
        test!(parse_20, "kern=oFf", b"kern", 0, ..);
    }
}

pub(crate) mod language {
    use core::str::FromStr;
    use smallvec::SmallVec;

    type SmallVecLanguage = SmallVec<[u8; 8]>;

    /// A language tag used to select shaping rules.
    #[derive(Clone, PartialEq, Eq, Hash, Debug)]
    pub struct Language(SmallVecLanguage);

    impl Language {
        /// Creates a language from nonempty bytes.
        ///
        /// ASCII letters are lowercased and underscores become hyphens. Returns
        /// `None` for empty input.
        #[inline]
        pub fn new(bytes: impl AsRef<[u8]>) -> Option<Self> {
            let bytes = bytes.as_ref();
            (!bytes.is_empty()).then(|| Language::from_bytes(bytes))
        }

        /// Returns the language as bytes.
        #[inline]
        pub fn as_bytes(&self) -> &[u8] {
            &self.0
        }

        /// Returns the language as UTF-8, or an empty string for invalid UTF-8.
        #[inline]
        pub fn as_str(&self) -> &str {
            core::str::from_utf8(&self.0).unwrap_or_default()
        }

        fn from_bytes(bytes: &[u8]) -> Self {
            if bytes.is_empty() {
                Language(SmallVec::new())
            } else {
                let mut bytes = SmallVecLanguage::from_slice(bytes);

                // Convert uppercase to lowercase and replace '_' with '-'.
                for b in &mut bytes.iter_mut() {
                    if b.is_ascii_uppercase() {
                        *b = b.to_ascii_lowercase();
                    } else if *b == b'_' {
                        *b = b'-';
                    }
                }

                Language(bytes)
            }
        }
    }

    impl FromStr for Language {
        type Err = &'static str;

        fn from_str(s: &str) -> Result<Self, Self::Err> {
            if !s.is_empty() {
                Ok(Language::from_bytes(s.as_bytes()))
            } else {
                Err("invalid language")
            }
        }
    }

    #[cfg(test)]
    mod tests_language {
        use super::*;
        use alloc::string::String;
        use alloc::vec::Vec;

        #[test]
        fn new_empty() {
            assert_eq!(Language::new(""), None);
        }

        #[test]
        fn new_basic() {
            let lang = Language::new("en").unwrap();
            assert_eq!(lang.as_str(), "en");
            assert_eq!(lang.as_bytes(), b"en");
        }

        #[test]
        fn new_lowercases() {
            let lang = Language::new("EN-US").unwrap();
            assert_eq!(lang.as_str(), "en-us");
            assert_eq!(lang.as_bytes(), b"en-us");
        }

        #[test]
        fn new_replaces_underscore() {
            let lang = Language::new("en_US").unwrap();
            assert_eq!(lang.as_str(), "en-us");
            assert_eq!(lang.as_bytes(), b"en-us");
        }

        #[test]
        fn new_accepts_str() {
            let lang = Language::new("zh-Hant").unwrap();
            assert_eq!(lang.as_str(), "zh-hant");
        }

        #[test]
        fn new_accepts_byte_slice() {
            let lang = Language::new(b"zh-Hant" as &[u8]).unwrap();
            assert_eq!(lang.as_str(), "zh-hant");
        }

        #[test]
        fn new_accepts_byte_array() {
            let lang = Language::new(*b"zh-Hant").unwrap();
            assert_eq!(lang.as_str(), "zh-hant");
        }

        #[test]
        fn new_accepts_string() {
            let lang = Language::new(String::from("zh-Hant")).unwrap();
            assert_eq!(lang.as_str(), "zh-hant");
        }

        #[test]
        fn new_accepts_vec() {
            let lang = Language::new(Vec::from(b"zh-Hant" as &[u8])).unwrap();
            assert_eq!(lang.as_str(), "zh-hant");
        }

        #[test]
        fn new_matches_from_str() {
            assert_eq!(
                Language::new("en-US"),
                Some(Language::from_str("en-US").unwrap())
            );
            assert_eq!(
                Language::new("zh_Hant_TW"),
                Some(Language::from_str("zh_Hant_TW").unwrap())
            );
        }

        #[test]
        fn new_empty_matches_from_str() {
            assert_eq!(Language::new(""), None);
            assert!(Language::from_str("").is_err());
        }
    }
}

pub(crate) mod options {
    use crate::{Feature, ShapePlan};

    /// Optional settings for a shaping operation.
    ///
    /// By default, shaping creates a plan, applies the font's default features,
    /// and has no point size for the `trak` table.
    #[derive(Default)]
    pub struct ShapeOptions<'a> {
        pub(crate) plan: Option<&'a ShapePlan>,
        pub(crate) point_size: Option<f32>,
        pub(crate) features: &'a [Feature],
    }

    impl<'a> ShapeOptions<'a> {
        /// Creates a default set of shape options ready for configuration.
        pub fn new() -> Self {
            Self::default()
        }

        /// Sets the plan to use for shaping.
        ///
        /// The shape plan must be compatible with the properties of the buffer
        /// passed to shaping.
        pub fn plan(mut self, plan: Option<&'a ShapePlan>) -> Self {
            self.plan = plan;
            self
        }

        /// Sets the point size used by the `trak` tracking table.
        pub fn point_size(mut self, point_size: Option<f32>) -> Self {
            self.point_size = point_size;
            self
        }

        /// Sets user features to apply during shaping.
        pub fn features(mut self, features: &'a [Feature]) -> Self {
            self.features = features;
            self
        }
    }
}

pub(crate) mod parse_setting {
    use crate::{text_parser::TextParser, Direction, Feature, Language, Script};
    use read_fonts::model::Variation;

    /// Parses a HarfBuzz-style setting from text.
    ///
    /// This includes font variations, whose type is defined by `read-fonts` and
    /// therefore cannot implement [`FromStr`](core::str::FromStr) here.
    pub trait ParseSetting: Sized {
        /// Parses a setting from its textual representation.
        ///
        /// # Errors
        ///
        /// Returns an error if the text is not a valid setting.
        fn parse_setting(s: &str) -> Result<Self, &'static str>;
    }

    impl ParseSetting for Direction {
        fn parse_setting(s: &str) -> Result<Self, &'static str> {
            s.parse()
        }
    }

    impl ParseSetting for Script {
        fn parse_setting(s: &str) -> Result<Self, &'static str> {
            s.parse()
        }
    }

    impl ParseSetting for Language {
        fn parse_setting(s: &str) -> Result<Self, &'static str> {
            s.parse()
        }
    }

    impl ParseSetting for Feature {
        fn parse_setting(s: &str) -> Result<Self, &'static str> {
            s.parse()
        }
    }

    impl ParseSetting for Variation {
        fn parse_setting(s: &str) -> Result<Self, &'static str> {
            fn parse(s: &str) -> Option<Variation> {
                if s.is_empty() {
                    return None;
                }

                let mut p = TextParser::new(s);
                p.skip_spaces();
                let quote = p.consume_quote();
                let tag = p.consume_tag()?;
                if let Some(quote) = quote {
                    p.consume_byte(quote)?;
                }
                let _ = p.consume_byte(b'=');
                let value = p.consume_f32()?;
                p.skip_spaces();
                p.at_end().then_some(Variation::new(tag, value))
            }

            parse(s).ok_or("invalid variation")
        }
    }
}

pub(crate) mod scale {
    use crate::GlyphExtents;
    use read_fonts::types::{BoundingBox, F48Dot16};

    #[derive(Copy, Clone)]
    /// Converts font units to the configured font scale.
    ///
    /// The conversion uses the scale set on [`ShaperFont`](crate::ShaperFont)
    /// and follows HarfBuzz's rounding rules for glyph metrics and shaping.
    #[derive(Debug)]
    pub struct Scale {
        x_mult: i64,
        y_mult: i64,
        x_multf: f32,
        y_multf: f32,
    }

    impl Default for Scale {
        fn default() -> Self {
            Self {
                x_mult: 1 << 16,
                y_mult: 1 << 16,
                x_multf: 1.0,
                y_multf: 1.0,
            }
        }
    }

    // Various conversions between f32 and i32
    #[allow(clippy::cast_precision_loss)]
    impl Scale {
        /// Creates a conversion from font units to the requested scale.
        ///
        /// Returns an identity conversion if `scale` is `None` or `upem` is zero.
        pub fn new(scale: Option<(i32, i32)>, upem: i32) -> Self {
            let (Some((x_scale, y_scale)), true) = (scale, upem != 0) else {
                // When scale is not configured, or upem is zero, return results
                // in font units.
                return Self::default();
            };
            let [x_mult, y_mult] = [x_scale, y_scale].map(|s| Self::mult_from_scale(s, upem));
            let upem = upem as f32;
            Self {
                x_mult,
                y_mult,
                x_multf: x_scale as f32 / upem,
                y_multf: y_scale as f32 / upem,
            }
        }

        /// Converts a horizontal distance from font units.
        #[inline(always)]
        pub fn scale_x(&self, x: i32) -> i32 {
            Self::scale_by_mult(x, self.x_mult)
        }

        /// Converts a vertical distance from font units.
        #[inline(always)]
        pub fn scale_y(&self, y: i32) -> i32 {
            Self::scale_by_mult(y, self.y_mult)
        }

        /// Scales a fractional (font-unit) value, matching HarfBuzz's `em_scalef`
        /// (`roundf(v * scale / upem)`).
        #[inline(always)]
        pub(crate) fn scale_x_f(&self, x: f32) -> i32 {
            (x * self.x_multf).round() as i32
        }

        #[inline(always)]
        pub(crate) fn scale_y_f(&self, y: f32) -> i32 {
            (y * self.y_multf).round() as i32
        }

        /// Clamps a scaled advance to the sign of its scale. This applies the
        /// unsigned font-unit bound while preserving deliberately negative scales.
        pub(crate) fn clamp_x_advance(&self, advance: i32) -> i32 {
            Self::clamp_advance(advance, self.x_multf)
        }

        pub(crate) fn clamp_y_advance(&self, advance: i32) -> i32 {
            Self::clamp_advance(advance, self.y_multf)
        }

        fn clamp_advance(advance: i32, scale: f32) -> i32 {
            if scale < 0.0 {
                advance.min(0)
            } else {
                advance.max(0)
            }
        }

        /// Scales glyph extents using HarfBuzz's corner rounding.
        ///
        /// Floors the near corners and ceils the far corners before calculating
        /// width and height.
        pub fn scale_extents(&self, mut extents: GlyphExtents) -> GlyphExtents {
            let x1 = extents.x_bearing as f32 * self.x_multf;
            let y1 = extents.y_bearing as f32 * self.y_multf;
            let x2 =
                (i64::from(extents.x_bearing) + i64::from(extents.width)) as f32 * self.x_multf;
            let y2 =
                (i64::from(extents.y_bearing) + i64::from(extents.height)) as f32 * self.y_multf;
            let rx1 = x1.floor();
            let ry1 = y1.floor();
            let rx2 = x2.ceil();
            let ry2 = y2.ceil();
            extents.x_bearing = rx1 as i32;
            extents.y_bearing = ry1 as i32;
            extents.width = (f64::from(rx2) - f64::from(rx1)) as i32;
            extents.height = (f64::from(ry2) - f64::from(ry1)) as i32;
            extents
        }

        #[inline(always)]
        fn mult_from_scale(scale: i32, upem: i32) -> i64 {
            if scale < 0 {
                -((-(scale as i64)) << 16) / upem as i64
            } else {
                ((scale as i64) << 16) / upem as i64
            }
        }

        #[inline(always)]
        fn scale_by_mult(value: i32, mult: i64) -> i32 {
            ((i64::from(value) * mult + 32768) >> 16) as i32
        }
    }

    impl read_fonts::model::metrics::Scale for Scale {
        type Value = i32;

        fn add(a: i32, b: i32) -> i32 {
            a.saturating_add(b)
        }

        fn sub(a: i32, b: i32) -> i32 {
            a.saturating_sub(b)
        }

        fn half(value: i32) -> i32 {
            value >> 1
        }

        fn scale_x(&self, value: F48Dot16) -> i32 {
            if value.to_bits().trailing_zeros() >= 16 {
                self.scale_x(value.to_i32())
            } else {
                self.scale_x_f(value.to_f32())
            }
        }

        fn scale_y(&self, value: F48Dot16) -> i32 {
            if value.to_bits().trailing_zeros() >= 16 {
                self.scale_y(value.to_i32())
            } else {
                self.scale_y_f(value.to_f32())
            }
        }

        fn scale_glyph_extents(
            &self,
            extents: read_fonts::model::metrics::GlyphExtents<F48Dot16>,
        ) -> read_fonts::model::metrics::GlyphExtents<i32> {
            let left = (extents.x_bearing.to_f32() * self.x_multf).floor();
            let top = (extents.y_bearing.to_f32() * self.y_multf).floor();
            let right = ((extents.x_bearing + extents.width).to_f32() * self.x_multf).ceil();
            let bottom = ((extents.y_bearing - extents.height).to_f32() * self.y_multf).ceil();
            read_fonts::model::metrics::GlyphExtents {
                x_bearing: left as i32,
                y_bearing: top as i32,
                width: (f64::from(right) - f64::from(left)) as i32,
                height: (f64::from(top) - f64::from(bottom)) as i32,
            }
        }

        fn scale_rect(&self, bounds: BoundingBox<F48Dot16>) -> BoundingBox<i32> {
            BoundingBox {
                x_min: self.scale_x_f(bounds.x_min.to_f32()),
                y_min: self.scale_y_f(bounds.y_min.to_f32()),
                x_max: self.scale_x_f(bounds.x_max.to_f32()),
                y_max: self.scale_y_f(bounds.y_max.to_f32()),
            }
        }
    }
}

pub(crate) mod script {
    use crate::{tag::TagExt, Tag};
    use core::str::FromStr;

    // In harfbuzz, despite having `Script`, script can actually have any tag.
    // So we're doing the same.
    // The only difference is that `Script` cannot be set to `HB_SCRIPT_INVALID`.
    /// A text script identified by an ISO 15924 tag.
    #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
    pub struct Script(pub(crate) Tag);

    impl Script {
        #[inline]
        pub(crate) const fn from_bytes(bytes: &[u8; 4]) -> Self {
            Script(Tag::new(bytes))
        }

        /// Converts an ISO 15924 script tag to a corresponding `Script`.
        pub const fn from_iso15924_tag(tag: Tag) -> Option<Script> {
            let tag = u32::from_be_bytes(tag.to_be_bytes());

            if tag == 0 {
                return None;
            }

            // Be lenient, adjust case (one capital letter followed by three small letters).
            let tag = (tag & 0xDFDF_DFDF) | 0x0020_2020;

            if tag & 0xE0E0_E0E0 != 0x4060_6060 {
                return Some(Script::UNKNOWN);
            }

            Some(match &tag.to_be_bytes() {
                // Script variants from https://unicode.org/iso15924/
                b"Aran" => Script::ARABIC,
                b"Cyrs" => Script::CYRILLIC,
                b"Geok" => Script::GEORGIAN,
                b"Hans" | b"Hant" => Script::HAN,
                b"Jamo" => Script::HANGUL,
                b"Latf" | b"Latg" => Script::LATIN,
                b"Syre" | b"Syrj" | b"Syrn" => Script::SYRIAC,
                &t => Script(Tag::from_be_bytes(t)),
            })
        }

        /// Returns script's tag.
        #[inline]
        pub fn tag(&self) -> Tag {
            self.0
        }
    }

    impl FromStr for Script {
        type Err = &'static str;

        fn from_str(s: &str) -> Result<Self, Self::Err> {
            let tag = Tag::from_bytes_lossy(s.as_bytes());
            Script::from_iso15924_tag(tag).ok_or("invalid script")
        }
    }

    #[allow(missing_docs)]
    impl Script {
        // Since 1.1
        pub const COMMON: Self = Self::from_bytes(b"Zyyy");
        pub const INHERITED: Self = Self::from_bytes(b"Zinh");
        pub const ARABIC: Self = Self::from_bytes(b"Arab");
        pub const ARMENIAN: Self = Self::from_bytes(b"Armn");
        pub const BENGALI: Self = Self::from_bytes(b"Beng");
        pub const CYRILLIC: Self = Self::from_bytes(b"Cyrl");
        pub const DEVANAGARI: Self = Self::from_bytes(b"Deva");
        pub const GEORGIAN: Self = Self::from_bytes(b"Geor");
        pub const GREEK: Self = Self::from_bytes(b"Grek");
        pub const GUJARATI: Self = Self::from_bytes(b"Gujr");
        pub const GURMUKHI: Self = Self::from_bytes(b"Guru");
        pub const HANGUL: Self = Self::from_bytes(b"Hang");
        pub const HAN: Self = Self::from_bytes(b"Hani");
        pub const HEBREW: Self = Self::from_bytes(b"Hebr");
        pub const HIRAGANA: Self = Self::from_bytes(b"Hira");
        pub const KANNADA: Self = Self::from_bytes(b"Knda");
        pub const KATAKANA: Self = Self::from_bytes(b"Kana");
        pub const LAO: Self = Self::from_bytes(b"Laoo");
        pub const LATIN: Self = Self::from_bytes(b"Latn");
        pub const MALAYALAM: Self = Self::from_bytes(b"Mlym");
        pub const ORIYA: Self = Self::from_bytes(b"Orya");
        pub const TAMIL: Self = Self::from_bytes(b"Taml");
        pub const TELUGU: Self = Self::from_bytes(b"Telu");
        pub const THAI: Self = Self::from_bytes(b"Thai");
        // Since 2.0
        pub const TIBETAN: Self = Self::from_bytes(b"Tibt");
        // Since 3.0
        pub const BOPOMOFO: Self = Self::from_bytes(b"Bopo");
        pub const BRAILLE: Self = Self::from_bytes(b"Brai");
        pub const CANADIAN_SYLLABICS: Self = Self::from_bytes(b"Cans");
        pub const CHEROKEE: Self = Self::from_bytes(b"Cher");
        pub const ETHIOPIC: Self = Self::from_bytes(b"Ethi");
        pub const KHMER: Self = Self::from_bytes(b"Khmr");
        pub const MONGOLIAN: Self = Self::from_bytes(b"Mong");
        pub const MYANMAR: Self = Self::from_bytes(b"Mymr");
        pub const OGHAM: Self = Self::from_bytes(b"Ogam");
        pub const RUNIC: Self = Self::from_bytes(b"Runr");
        pub const SINHALA: Self = Self::from_bytes(b"Sinh");
        pub const SYRIAC: Self = Self::from_bytes(b"Syrc");
        pub const THAANA: Self = Self::from_bytes(b"Thaa");
        pub const YI: Self = Self::from_bytes(b"Yiii");
        // Since 3.1
        pub const DESERET: Self = Self::from_bytes(b"Dsrt");
        pub const GOTHIC: Self = Self::from_bytes(b"Goth");
        pub const OLD_ITALIC: Self = Self::from_bytes(b"Ital");
        // Since 3.2
        pub const BUHID: Self = Self::from_bytes(b"Buhd");
        pub const HANUNOO: Self = Self::from_bytes(b"Hano");
        pub const TAGALOG: Self = Self::from_bytes(b"Tglg");
        pub const TAGBANWA: Self = Self::from_bytes(b"Tagb");
        // Since 4.0
        pub const CYPRIOT: Self = Self::from_bytes(b"Cprt");
        pub const LIMBU: Self = Self::from_bytes(b"Limb");
        pub const LINEAR_B: Self = Self::from_bytes(b"Linb");
        pub const OSMANYA: Self = Self::from_bytes(b"Osma");
        pub const SHAVIAN: Self = Self::from_bytes(b"Shaw");
        pub const TAI_LE: Self = Self::from_bytes(b"Tale");
        pub const UGARITIC: Self = Self::from_bytes(b"Ugar");
        // Since 4.1
        pub const BUGINESE: Self = Self::from_bytes(b"Bugi");
        pub const COPTIC: Self = Self::from_bytes(b"Copt");
        pub const GLAGOLITIC: Self = Self::from_bytes(b"Glag");
        pub const KHAROSHTHI: Self = Self::from_bytes(b"Khar");
        pub const NEW_TAI_LUE: Self = Self::from_bytes(b"Talu");
        pub const OLD_PERSIAN: Self = Self::from_bytes(b"Xpeo");
        pub const SYLOTI_NAGRI: Self = Self::from_bytes(b"Sylo");
        pub const TIFINAGH: Self = Self::from_bytes(b"Tfng");
        // Since 5.0
        pub const UNKNOWN: Self = Self::from_bytes(b"Zzzz"); // Script can be Unknown, but not Invalid.
        pub const BALINESE: Self = Self::from_bytes(b"Bali");
        pub const CUNEIFORM: Self = Self::from_bytes(b"Xsux");
        pub const NKO: Self = Self::from_bytes(b"Nkoo");
        pub const PHAGS_PA: Self = Self::from_bytes(b"Phag");
        pub const PHOENICIAN: Self = Self::from_bytes(b"Phnx");
        // Since 5.1
        pub const CARIAN: Self = Self::from_bytes(b"Cari");
        pub const CHAM: Self = Self::from_bytes(b"Cham");
        pub const KAYAH_LI: Self = Self::from_bytes(b"Kali");
        pub const LEPCHA: Self = Self::from_bytes(b"Lepc");
        pub const LYCIAN: Self = Self::from_bytes(b"Lyci");
        pub const LYDIAN: Self = Self::from_bytes(b"Lydi");
        pub const OL_CHIKI: Self = Self::from_bytes(b"Olck");
        pub const REJANG: Self = Self::from_bytes(b"Rjng");
        pub const SAURASHTRA: Self = Self::from_bytes(b"Saur");
        pub const SUNDANESE: Self = Self::from_bytes(b"Sund");
        pub const VAI: Self = Self::from_bytes(b"Vaii");
        // Since 5.2
        pub const AVESTAN: Self = Self::from_bytes(b"Avst");
        pub const BAMUM: Self = Self::from_bytes(b"Bamu");
        pub const EGYPTIAN_HIEROGLYPHS: Self = Self::from_bytes(b"Egyp");
        pub const IMPERIAL_ARAMAIC: Self = Self::from_bytes(b"Armi");
        pub const INSCRIPTIONAL_PAHLAVI: Self = Self::from_bytes(b"Phli");
        pub const INSCRIPTIONAL_PARTHIAN: Self = Self::from_bytes(b"Prti");
        pub const JAVANESE: Self = Self::from_bytes(b"Java");
        pub const KAITHI: Self = Self::from_bytes(b"Kthi");
        pub const LISU: Self = Self::from_bytes(b"Lisu");
        pub const MEETEI_MAYEK: Self = Self::from_bytes(b"Mtei");
        pub const OLD_SOUTH_ARABIAN: Self = Self::from_bytes(b"Sarb");
        pub const OLD_TURKIC: Self = Self::from_bytes(b"Orkh");
        pub const SAMARITAN: Self = Self::from_bytes(b"Samr");
        pub const TAI_THAM: Self = Self::from_bytes(b"Lana");
        pub const TAI_VIET: Self = Self::from_bytes(b"Tavt");
        // Since 6.0
        pub const BATAK: Self = Self::from_bytes(b"Batk");
        pub const BRAHMI: Self = Self::from_bytes(b"Brah");
        pub const MANDAIC: Self = Self::from_bytes(b"Mand");
        // Since 6.1
        pub const CHAKMA: Self = Self::from_bytes(b"Cakm");
        pub const MEROITIC_CURSIVE: Self = Self::from_bytes(b"Merc");
        pub const MEROITIC_HIEROGLYPHS: Self = Self::from_bytes(b"Mero");
        pub const MIAO: Self = Self::from_bytes(b"Plrd");
        pub const SHARADA: Self = Self::from_bytes(b"Shrd");
        pub const SORA_SOMPENG: Self = Self::from_bytes(b"Sora");
        pub const TAKRI: Self = Self::from_bytes(b"Takr");
        // Since 7.0
        pub const BASSA_VAH: Self = Self::from_bytes(b"Bass");
        pub const CAUCASIAN_ALBANIAN: Self = Self::from_bytes(b"Aghb");
        pub const DUPLOYAN: Self = Self::from_bytes(b"Dupl");
        pub const ELBASAN: Self = Self::from_bytes(b"Elba");
        pub const GRANTHA: Self = Self::from_bytes(b"Gran");
        pub const KHOJKI: Self = Self::from_bytes(b"Khoj");
        pub const KHUDAWADI: Self = Self::from_bytes(b"Sind");
        pub const LINEAR_A: Self = Self::from_bytes(b"Lina");
        pub const MAHAJANI: Self = Self::from_bytes(b"Mahj");
        pub const MANICHAEAN: Self = Self::from_bytes(b"Mani");
        pub const MENDE_KIKAKUI: Self = Self::from_bytes(b"Mend");
        pub const MODI: Self = Self::from_bytes(b"Modi");
        pub const MRO: Self = Self::from_bytes(b"Mroo");
        pub const NABATAEAN: Self = Self::from_bytes(b"Nbat");
        pub const OLD_NORTH_ARABIAN: Self = Self::from_bytes(b"Narb");
        pub const OLD_PERMIC: Self = Self::from_bytes(b"Perm");
        pub const PAHAWH_HMONG: Self = Self::from_bytes(b"Hmng");
        pub const PALMYRENE: Self = Self::from_bytes(b"Palm");
        pub const PAU_CIN_HAU: Self = Self::from_bytes(b"Pauc");
        pub const PSALTER_PAHLAVI: Self = Self::from_bytes(b"Phlp");
        pub const SIDDHAM: Self = Self::from_bytes(b"Sidd");
        pub const TIRHUTA: Self = Self::from_bytes(b"Tirh");
        pub const WARANG_CITI: Self = Self::from_bytes(b"Wara");
        // Since 8.0
        pub const AHOM: Self = Self::from_bytes(b"Ahom");
        pub const ANATOLIAN_HIEROGLYPHS: Self = Self::from_bytes(b"Hluw");
        pub const HATRAN: Self = Self::from_bytes(b"Hatr");
        pub const MULTANI: Self = Self::from_bytes(b"Mult");
        pub const OLD_HUNGARIAN: Self = Self::from_bytes(b"Hung");
        pub const SIGNWRITING: Self = Self::from_bytes(b"Sgnw");
        // Since 9.0
        pub const ADLAM: Self = Self::from_bytes(b"Adlm");
        pub const BHAIKSUKI: Self = Self::from_bytes(b"Bhks");
        pub const MARCHEN: Self = Self::from_bytes(b"Marc");
        pub const OSAGE: Self = Self::from_bytes(b"Osge");
        pub const TANGUT: Self = Self::from_bytes(b"Tang");
        pub const NEWA: Self = Self::from_bytes(b"Newa");
        // Since 10.0
        pub const MASARAM_GONDI: Self = Self::from_bytes(b"Gonm");
        pub const NUSHU: Self = Self::from_bytes(b"Nshu");
        pub const SOYOMBO: Self = Self::from_bytes(b"Soyo");
        pub const ZANABAZAR_SQUARE: Self = Self::from_bytes(b"Zanb");
        // Since 11.0
        pub const DOGRA: Self = Self::from_bytes(b"Dogr");
        pub const GUNJALA_GONDI: Self = Self::from_bytes(b"Gong");
        pub const HANIFI_ROHINGYA: Self = Self::from_bytes(b"Rohg");
        pub const MAKASAR: Self = Self::from_bytes(b"Maka");
        pub const MEDEFAIDRIN: Self = Self::from_bytes(b"Medf");
        pub const OLD_SOGDIAN: Self = Self::from_bytes(b"Sogo");
        pub const SOGDIAN: Self = Self::from_bytes(b"Sogd");
        // Since 12.0
        pub const ELYMAIC: Self = Self::from_bytes(b"Elym");
        pub const NANDINAGARI: Self = Self::from_bytes(b"Nand");
        pub const NYIAKENG_PUACHUE_HMONG: Self = Self::from_bytes(b"Hmnp");
        pub const WANCHO: Self = Self::from_bytes(b"Wcho");
        // Since 13.0
        pub const CHORASMIAN: Self = Self::from_bytes(b"Chrs");
        pub const DIVES_AKURU: Self = Self::from_bytes(b"Diak");
        pub const KHITAN_SMALL_SCRIPT: Self = Self::from_bytes(b"Kits");
        pub const YEZIDI: Self = Self::from_bytes(b"Yezi");
        // Since 14.0
        pub const CYPRO_MINOAN: Self = Self::from_bytes(b"Cpmn");
        pub const OLD_UYGHUR: Self = Self::from_bytes(b"Ougr");
        pub const TANGSA: Self = Self::from_bytes(b"Tnsa");
        pub const TOTO: Self = Self::from_bytes(b"Toto");
        pub const VITHKUQI: Self = Self::from_bytes(b"Vith");
        // Since 15.0
        pub const KAWI: Self = Self::from_bytes(b"Kawi");
        pub const NAG_MUNDARI: Self = Self::from_bytes(b"Nagm");
        // Since 16.0
        pub const GARAY: Self = Self::from_bytes(b"Gara");
        pub const GURUNG_KHEMA: Self = Self::from_bytes(b"Gukh");
        pub const KIRAT_RAI: Self = Self::from_bytes(b"Krai");
        pub const OL_ONAL: Self = Self::from_bytes(b"Onao");
        pub const SUNUWAR: Self = Self::from_bytes(b"Sunu");
        pub const TODHRI: Self = Self::from_bytes(b"Todr");
        pub const TULU_TIGALARI: Self = Self::from_bytes(b"Tutg");
        // Since 17.0
        pub const BERIA_ERFE: Self = Self::from_bytes(b"Berf");
        pub const SIDETIC: Self = Self::from_bytes(b"Sidt");
        pub const TAI_YO: Self = Self::from_bytes(b"Tayo");
        pub const TOLONG_SIKI: Self = Self::from_bytes(b"Tols");
        // Since 18.0
        pub const JURCHEN: Self = Self::from_bytes(b"Jurc");
        pub const PROTO_CUNEIFORM: Self = Self::from_bytes(b"Pcun");
        pub const SEAL: Self = Self::from_bytes(b"Seal");

        pub const MATH: Self = Self::from_bytes(b"Zmth");

        // https://github.com/harfbuzz/harfbuzz/issues/1162
        pub const MYANMAR_ZAWGYI: Self = Self::from_bytes(b"Qaag");
    }
}

pub(crate) mod text_parser {
    use crate::{tag::TagExt, Tag};

    pub struct TextParser<'a> {
        pos: usize,
        text: &'a str,
    }

    impl<'a> TextParser<'a> {
        #[inline]
        pub fn new(text: &'a str) -> Self {
            TextParser { pos: 0, text }
        }

        #[inline]
        pub fn at_end(&self) -> bool {
            self.pos >= self.text.len()
        }

        #[inline]
        pub fn curr_byte(&self) -> Option<u8> {
            if !self.at_end() {
                Some(self.curr_byte_unchecked())
            } else {
                None
            }
        }

        #[inline]
        fn curr_byte_unchecked(&self) -> u8 {
            self.text.as_bytes()[self.pos]
        }

        #[inline]
        pub fn advance(&mut self, n: usize) {
            debug_assert!(self.pos + n <= self.text.len());
            self.pos += n;
        }

        pub fn consume_byte(&mut self, c: u8) -> Option<()> {
            let curr = self.curr_byte()?;
            if curr != c {
                return None;
            }

            self.advance(1);
            Some(())
        }

        #[inline]
        pub fn skip_spaces(&mut self) {
            // Unlike harfbuzz::ISSPACE, is_ascii_whitespace doesn't includes `\v`, but whatever.
            while !self.at_end() && self.curr_byte_unchecked().is_ascii_whitespace() {
                self.advance(1);
            }
        }

        pub fn consume_quote(&mut self) -> Option<u8> {
            let c = self.curr_byte()?;
            if matches!(c, b'\'' | b'"') {
                self.advance(1);
                Some(c)
            } else {
                None
            }
        }

        #[inline]
        pub fn consume_bytes<F>(&mut self, f: F) -> &'a str
        where
            F: Fn(u8) -> bool,
        {
            let start = self.pos;
            self.skip_bytes(f);
            &self.text[start..self.pos]
        }

        pub fn skip_bytes<F>(&mut self, f: F)
        where
            F: Fn(u8) -> bool,
        {
            while !self.at_end() && f(self.curr_byte_unchecked()) {
                self.advance(1);
            }
        }

        pub fn consume_tag(&mut self) -> Option<Tag> {
            let tag = self.consume_bytes(|c| c.is_ascii_alphanumeric() || c == b'_');
            if tag.len() > 4 {
                return None;
            }

            Some(Tag::from_bytes_lossy(tag.as_bytes()))
        }

        pub fn consume_i32(&mut self) -> Option<i32> {
            let start = self.pos;

            if matches!(self.curr_byte(), Some(b'-' | b'+')) {
                self.advance(1);
            }

            self.skip_bytes(|c| c.is_ascii_digit());
            self.text[start..self.pos].parse::<i32>().ok()
        }

        pub fn consume_f32(&mut self) -> Option<f32> {
            let start = self.pos;

            // TODO: does number like 1-e2 required?

            if matches!(self.curr_byte(), Some(b'-' | b'+')) {
                self.advance(1);
            }

            self.skip_bytes(|c| c.is_ascii_digit());

            if self.consume_byte(b'.').is_some() {
                self.skip_bytes(|c| c.is_ascii_digit());
            }

            self.text[start..self.pos].parse::<f32>().ok()
        }

        pub fn consume_bool(&mut self) -> Option<bool> {
            self.skip_spaces();

            let value = self.consume_bytes(|c| c.is_ascii_alphabetic()).as_bytes();
            if value.len() == 2 {
                if value[0].eq_ignore_ascii_case(&b'o') && value[1].eq_ignore_ascii_case(&b'n') {
                    return Some(true);
                }
            } else if value.len() == 3 {
                if value[0].eq_ignore_ascii_case(&b'o')
                    && value[1].eq_ignore_ascii_case(&b'f')
                    && value[2].eq_ignore_ascii_case(&b'f')
                {
                    return Some(false);
                }
            }

            None
        }
    }
}
