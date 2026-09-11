pub(crate) use super::microphone::{Recording, start_recording};
use std::{os::unix::process::CommandExt, process::Command};

pub(super) fn prepare_host(command: &mut Command) {
    command.process_group(0);
}
