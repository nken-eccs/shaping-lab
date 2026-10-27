use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Corpus {
    pub schema_version: u32,
    pub fonts: BTreeMap<String, FontSpec>,
    pub cases: Vec<Case>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FontSpec {
    pub path: PathBuf,
    #[serde(default)]
    pub index: u32,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub description: String,
    pub runs: Vec<Run>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub font: String,
    pub text: String,
    pub direction: Direction,
    pub script: String,
    pub language: String,
    #[serde(default)]
    pub features: Vec<Feature>,
    #[serde(default)]
    pub variations: BTreeMap<String, f32>,
    #[serde(default)]
    pub cluster_level: ClusterLevel,
    #[serde(default)]
    pub flags: Vec<BufferFlag>,
    #[serde(default)]
    pub pre_context: String,
    #[serde(default)]
    pub post_context: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Feature {
    pub tag: String,
    pub value: u32,
    #[serde(default)]
    pub start: u32,
    #[serde(default = "feature_end")]
    pub end: u32,
}

fn feature_end() -> u32 {
    u32::MAX
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Ltr,
    Rtl,
    Ttb,
    Btt,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClusterLevel {
    #[default]
    MonotoneGraphemes,
    MonotoneCharacters,
    Characters,
    Graphemes,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BufferFlag {
    Bot,
    Eot,
    PreserveDefaultIgnorables,
    RemoveDefaultIgnorables,
    DoNotInsertDottedCircle,
    UnsafeToConcat,
    SafeToInsertTatweel,
}

impl Run {
    pub fn properties(&self) -> Result<crate::shaper::Properties> {
        use shaping_core as shaping;
        Ok(crate::shaper::Properties {
            direction: match self.direction {
                Direction::Ltr => shaping::Direction::LeftToRight,
                Direction::Rtl => shaping::Direction::RightToLeft,
                Direction::Ttb => shaping::Direction::TopToBottom,
                Direction::Btt => shaping::Direction::BottomToTop,
            },
            script: Some(
                self.script
                    .parse()
                    .map_err(|_| anyhow::anyhow!("Invalid script"))?,
            ),
            language: Some(
                self.language
                    .parse()
                    .map_err(|_| anyhow::anyhow!("Invalid language"))?,
            ),
            features: self
                .features
                .iter()
                .map(|feature| {
                    Ok(shaping::Feature {
                        tag: shaping::Tag::new(
                            feature
                                .tag
                                .as_bytes()
                                .try_into()
                                .context("Invalid feature tag")?,
                        ),
                        value: feature.value,
                        start: feature.start,
                        end: feature.end,
                    })
                })
                .collect::<Result<_>>()?,
            cluster_level: match self.cluster_level {
                ClusterLevel::MonotoneGraphemes => shaping::ClusterLevel::MonotoneGraphemes,
                ClusterLevel::MonotoneCharacters => shaping::ClusterLevel::MonotoneCharacters,
                ClusterLevel::Characters => shaping::ClusterLevel::Characters,
                ClusterLevel::Graphemes => shaping::ClusterLevel::Graphemes,
            },
            flags: shaping::BufferFlags::from_bits_retain(self.flag_bits()),
        })
    }

    pub fn flag_bits(&self) -> u32 {
        self.flags.iter().fold(0, |bits, flag| {
            bits | match flag {
                BufferFlag::Bot => 1,
                BufferFlag::Eot => 2,
                BufferFlag::PreserveDefaultIgnorables => 4,
                BufferFlag::RemoveDefaultIgnorables => 8,
                BufferFlag::DoNotInsertDottedCircle => 16,
                BufferFlag::UnsafeToConcat => 64,
                BufferFlag::SafeToInsertTatweel => 128,
            }
        })
    }
}

impl Case {
    pub fn bytes(&self) -> usize {
        self.runs.iter().map(|r| r.text.len()).sum()
    }
    pub fn chars(&self) -> usize {
        self.runs.iter().map(|r| r.text.chars().count()).sum()
    }
}

pub struct LoadedFont {
    pub bytes: Arc<Vec<u8>>,
    pub index: u32,
    pub sha256: String,
}

impl LoadedFont {
    pub fn instantiate(&self, variations: &BTreeMap<String, f32>) -> Result<shaping_core::Font> {
        let font = shaping_core::Font::new(
            shaping_core::font::Blob::Shared(self.bytes.clone()),
            self.index,
        )
        .context("Invalid font or face index")?;
        if variations.is_empty() {
            return Ok(font);
        }
        let variations = variations
            .iter()
            .map(|(tag, value)| {
                ensure!(value.is_finite(), "Invalid variation coordinate");
                Ok((
                    shaping_core::Tag::new(
                        tag.as_bytes().try_into().context("Invalid variation tag")?,
                    ),
                    *value,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(font.instance_builder().variations(variations).build())
    }
}

pub struct LoadedCorpus {
    pub spec: Corpus,
    pub fonts: BTreeMap<String, LoadedFont>,
    pub sha256: String,
    pub path: PathBuf,
}

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn tag_valid(tag: &str) -> bool {
    tag.len() == 4 && tag.bytes().all(|c| (b' '..=b'~').contains(&c))
}

impl Corpus {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == 1,
            "Unsupported schema_version: {}",
            self.schema_version
        );
        ensure!(!self.cases.is_empty(), "cases must not be empty");
        let mut ids = HashSet::new();
        for case in &self.cases {
            ensure!(
                !case.id.is_empty()
                    && case
                        .id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c)),
                "Invalid case id: {}",
                case.id
            );
            ensure!(ids.insert(&case.id), "Duplicate case id: {}", case.id);
            ensure!(!case.runs.is_empty(), "{}: runs must not be empty", case.id);
            for run in &case.runs {
                ensure!(
                    self.fonts.contains_key(&run.font),
                    "{}: unknown font: {}",
                    case.id,
                    run.font
                );
                ensure!(
                    run.script.len() == 4 && run.script.bytes().all(|c| c.is_ascii_alphabetic()),
                    "{}: script must be a four-letter ISO 15924 tag",
                    case.id
                );
                ensure!(
                    !run.language.is_empty()
                        && run
                            .language
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || c == b'-'),
                    "{}: invalid language",
                    case.id
                );
                ensure!(
                    run.text.len() + run.pre_context.len() + run.post_context.len()
                        <= i32::MAX as usize,
                    "{}: text exceeds the C API length limit",
                    case.id
                );
                ensure!(
                    run.flag_bits() & 12 != 12,
                    "{}: preserve and remove default ignorables are mutually exclusive",
                    case.id
                );
                for f in &run.features {
                    ensure!(tag_valid(&f.tag), "{}: invalid feature tag", case.id);
                    ensure!(
                        f.start <= f.end && run.text.is_char_boundary(f.start as usize),
                        "{}: invalid feature start",
                        case.id
                    );
                    ensure!(
                        f.end == u32::MAX || run.text.is_char_boundary(f.end as usize),
                        "{}: invalid feature end",
                        case.id
                    );
                }
                for (tag, value) in &run.variations {
                    ensure!(
                        tag_valid(tag) && value.is_finite(),
                        "{}: invalid variation",
                        case.id
                    );
                }
            }
        }
        Ok(())
    }
}

impl LoadedCorpus {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let data = fs::read(path).with_context(|| format!("Cannot read {}", path.display()))?;
        let spec: Corpus = toml::from_str(std::str::from_utf8(&data)?)?;
        spec.validate()?;
        let base = path.parent().unwrap_or(Path::new("."));
        let mut fonts = BTreeMap::new();
        for (name, font) in &spec.fonts {
            let font_path = base.join(&font.path);
            let bytes = fs::read(&font_path).with_context(|| {
                format!(
                    "Font not found: {}\npython3 lab.py fetch",
                    font_path.display()
                )
            })?;
            ensure!(
                bytes.len() <= u32::MAX as usize,
                "Font exceeds the C API length limit: {name}"
            );
            let hash = sha256(&bytes);
            if let Some(expected) = &font.sha256 {
                ensure!(hash == *expected, "Font SHA-256 mismatch: {name}");
            }
            fonts.insert(
                name.clone(),
                LoadedFont {
                    bytes: Arc::new(bytes),
                    index: font.index,
                    sha256: hash,
                },
            );
        }
        Ok(Self {
            spec,
            fonts,
            sha256: sha256(&data),
            path: path.to_path_buf(),
        })
    }

    pub fn cases(&self, filter: Option<&str>) -> Result<Vec<&Case>> {
        let cases: Vec<_> = self
            .spec
            .cases
            .iter()
            .filter(|case| filter.is_none_or(|f| case.id == f))
            .collect();
        ensure!(
            !cases.is_empty(),
            "Case not found: {}",
            filter.unwrap_or_default()
        );
        Ok(cases)
    }
}
