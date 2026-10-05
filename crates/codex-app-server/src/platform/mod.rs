#[cfg(unix)]
use std::fs;
use std::{borrow::Cow, path::Path};

pub(crate) fn executable_path(path: &Path) -> Cow<'_, Path> {
    #[cfg(windows)]
    if path.extension().is_none() {
        return Cow::Owned(path.with_extension("exe"));
    }
    Cow::Borrowed(path)
}

pub(crate) fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}
