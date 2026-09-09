pub(crate) use super::microphone::{Recording, start_recording};
use std::{fs::OpenOptions, os::windows::process::CommandExt, process::Command};

pub(super) fn prepare_host(command: &mut Command) {
    // CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW: daemon outlives its view.
    command.creation_flags(0x00000200 | 0x08000000);
}

pub(super) fn private_file_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    options
}
