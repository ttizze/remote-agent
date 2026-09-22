use anyhow::{Context, Result};
use std::{
    env,
    path::{Path, PathBuf},
};

pub fn home() -> Result<PathBuf> {
    env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .context("HOME is not set")
}

pub fn expand_path(path: &Path) -> Result<PathBuf> {
    Ok(if let Ok(suffix) = path.strip_prefix("~") {
        home()?.join(suffix)
    } else {
        path.to_owned()
    })
}
