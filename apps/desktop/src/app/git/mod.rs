//! Desktop Git controls. Git state and action choices come from agent-core;
//! this module only renders the compact branch/action toolbar.
mod control;
mod publish;
mod toolbar;

pub(super) use toolbar::render;
