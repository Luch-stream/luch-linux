//! Luch's latency lab: photon-to-photon latency from a phone's 240 fps video of the computer and the TV.
//!
//! The sender shows a pattern (`pattern`) that changes every vsync, the TV shows it mirrored next to a pattern of its
//! own, a camera films both, and `analyze` reads the video back. `synth` is a camera that never was, for tests.
//! See docs/latency-lab.md.

pub mod analyze;
pub mod cli;
pub mod geom;
pub mod pattern;
pub mod report;
pub mod synth;
pub mod video;

pub use analyze::{analyze, Analysis, Options};
