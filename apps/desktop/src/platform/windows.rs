pub(crate) use super::microphone::{Recording, start_recording};
use std::{os::windows::process::CommandExt, process::Command};

pub(super) fn prepare_host(command: &mut Command) {
    // CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW: daemon outlives its view.
    command.creation_flags(0x00000200 | 0x08000000);
}
