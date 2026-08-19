//! Pure manifest/lock parsing for the dependency-health scanner
//! (`crate::use_cases::DependencyHealthScanner`).
//!
//! These functions turn ALREADY-read file *contents* into typed records so the
//! scanner decides health purely over data snapshots read through
//! `WorkspaceFilesPort`, never touching disk or network itself — keeping the
//! hexagonal ratchet green.
pub mod manifest;
