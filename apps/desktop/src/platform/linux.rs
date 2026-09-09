pub(crate) use super::microphone::{Recording, start_recording};
use std::{
    fs::OpenOptions,
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    process::Command,
};

pub(super) fn prepare_host(command: &mut Command) {
    command.process_group(0);
}

pub(super) fn private_file_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    options
}
