//! Native library artifact; all exported types and behavior are owned by agent-core.
agent_core::uniffi_reexport_scaffolding!();

#[cfg(target_os = "ios")]
pub use agent_nsh::embedded_ios::*;
