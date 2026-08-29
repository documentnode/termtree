//! `resource-benchmark`: the resource-usage benchmark harness (spec at
//! `doc/spec/resource-benchmark-spec.md`, design at
//! `doc/design/resource-benchmark-design.md`).
//!
//! Structured as a library plus a thin `main.rs` binary so that every
//! module's public API is available to `main.rs`'s live orchestration
//! *and* to this crate's own test suite. `run.rs::RunOrchestrator::run` is
//! the live sweep `main.rs`'s `run_sweep` drives (design §5.8); it is
//! exercised through `doctor` and a live smoke sweep, not this crate's
//! unit tests -- design §11 explains what a measurement harness cannot
//! unit-test and why every decision it makes is factored into a pure,
//! tested function instead.

pub mod attribution;
pub mod bundle_paths;
pub mod cli;
pub mod cold_start;
pub mod cpu_sampler;
pub mod exec;
pub mod footprint;
pub mod host_memory;
pub mod launch_services;
pub mod log_marks;
pub mod process_tree;
pub mod provenance;
pub mod quiesce;
pub mod render;
pub mod result;
pub mod run;
pub mod scratch_home;
pub mod seeding;
pub mod settings;
pub mod stats;
pub mod subject;
pub mod tier;
pub mod window_probe;
