//! Execution, repository, workspace, integration, verification, and persistence mechanisms for Tenet.

pub mod backend;
pub mod git;
pub mod graph;
pub mod integration;
mod protection;
pub mod scheduler;
pub mod store;
pub mod trusted_verifier;
pub mod verifier;
pub mod workspace;
