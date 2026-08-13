//! AI provider integration: credential storage (ROADMAP.md Phase 3 — AI
//! Providers; ARCHITECTURE.md §5, §7).
//!
//! The infrastructure layer is responsible for AI providers and operating
//! system integration. This module currently contains only the provider
//! credential store over the OS secure keyring ([`credentials`], FR-014).
//! Request execution, response normalization, and retry handling are distinct
//! Phase 3 tasks and intentionally live elsewhere.

// This crate has no credential-store consumer yet (the application layer's
// provider credential service and Tauri commands arrive in later Phase 3
// tasks), so store items not yet referenced are intentionally unused. Remove
// this attribute once a consumer references them.
#![allow(dead_code)]

pub mod anthropic;
pub mod credentials;
pub mod gemini;
pub mod openai;
