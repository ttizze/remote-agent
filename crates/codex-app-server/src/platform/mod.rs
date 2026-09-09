#[cfg(unix)]
use std::fs;
use std::{borrow::Cow, io, path::Path};

pub(crate) fn bundled_codex_path() -> Option<&'static Path> {
    #[cfg(target_os = "macos")]
    {
        Some(Path::new(
            "/Applications/ChatGPT.app/Contents/Resources/codex",
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

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

pub(crate) fn schema_directory() -> io::Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("remote-agent-codex-schema-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    builder.tempdir()
}
