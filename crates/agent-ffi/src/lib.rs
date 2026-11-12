//! Native library artifact; all exported types and behavior are owned by agent-core.
agent_core::uniffi_reexport_scaffolding!();

#[cfg(all(target_os = "ios", feature = "nsh-conversation"))]
pub use agent_nsh::embedded_ios::*;
