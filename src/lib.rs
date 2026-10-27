#![doc = include_str!("../README.md")]

pub mod corpus;
pub mod engine;
pub mod report;
pub use shaping_core as shaping;
pub use shaping_core::reusable as shaper;

pub use corpus::{Case, Corpus, LoadedCorpus};
pub use engine::{Backend, FontSet, Mode, PlanMode, Session};
