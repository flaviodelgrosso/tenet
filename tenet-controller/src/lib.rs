//! Deterministic orchestration and trust-policy control plane for Tenet.
//!
//! This crate owns run sequencing, catalog authority, evidence lifecycle policy,
//! verification authorization, completion decisions, and the coding-agent port.
//! Execution, repository, workspace, integration, verification, and persistence
//! mechanisms remain in `tenet-runtime`.

pub mod catalog;
pub mod completion;
pub mod controller;
pub mod evidence;
pub mod ports;
pub mod verification;

pub use controller::Controller;
pub use ports::agent::AgentBackend;
