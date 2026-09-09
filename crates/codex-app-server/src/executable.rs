use std::{
    env,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use crate::{
    Error,
    platform::{bundled_codex_path, executable_path, is_executable},
};

pub(crate) const DEFAULT_CODEX_PROGRAM: &str = "codex";

pub(crate) fn resolve(program: &Path) -> Result<PathBuf, Error> {
    let path = env::var_os("PATH");
    resolve_from(program, path.as_deref(), bundled_codex_path())
}

fn resolve_from(
    program: &Path,
    path: Option<&OsStr>,
    bundled_codex: Option<&Path>,
) -> Result<PathBuf, Error> {
    if program.components().count() > 1 || program.is_absolute() {
        let resolved = fs::canonicalize(executable_path(program)).map_err(|source| {
            Error::ResolveExecutable {
                program: program.to_path_buf(),
                source,
            }
        })?;
        if is_executable(&resolved) {
            return Ok(resolved);
        }
        return Err(Error::ExecutableNotFound(program.to_path_buf()));
    }

    if program == Path::new(DEFAULT_CODEX_PROGRAM) {
        if let Some(resolved) = resolve_candidate(bundled_codex)? {
            return Ok(resolved);
        }
    }

    let path = path.ok_or_else(|| Error::ExecutableNotFound(program.to_path_buf()))?;
    for directory in env::split_paths(path) {
        let candidate = directory.join(executable_path(program));
        if let Some(resolved) = resolve_candidate(Some(&candidate))? {
            return Ok(resolved);
        }
    }
    Err(Error::ExecutableNotFound(program.to_path_buf()))
}

fn resolve_candidate(candidate: Option<&Path>) -> Result<Option<PathBuf>, Error> {
    let Some(candidate) = candidate else {
        return Ok(None);
    };
    if !is_executable(candidate) {
        return Ok(None);
    }
    fs::canonicalize(candidate)
        .map(Some)
        .map_err(|source| Error::ResolveExecutable {
            program: candidate.to_path_buf(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new(label: &str) -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            let path = env::temp_dir().join(format!(
                "remote-agent-codex-resolution-{label}-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).expect("create test directory");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn write_executable(path: &Path) {
        fs::write(path, b"not invoked by this test").expect("write test executable");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = fs::metadata(path).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).unwrap();
        }
    }

    fn path_variable(directory: &Path) -> std::ffi::OsString {
        env::join_paths([directory]).expect("build test PATH")
    }

    #[test]
    fn prefers_bundled_codex_before_path_candidates() {
        let root = TestDirectory::new("bundle-first");
        let bundled = root.path().join("ChatGPT.app/Contents/Resources/codex");
        let path_directory = root.path().join("path");
        fs::create_dir_all(bundled.parent().unwrap()).unwrap();
        fs::create_dir(&path_directory).unwrap();
        write_executable(&bundled);
        let path_codex = path_directory.join(executable_path(Path::new("codex")));
        write_executable(&path_codex);

        let resolved = resolve_from(
            Path::new("codex"),
            Some(path_variable(&path_directory).as_os_str()),
            Some(&bundled),
        )
        .unwrap();

        assert_eq!(resolved, fs::canonicalize(bundled).unwrap());
    }

    #[test]
    fn falls_back_to_path_when_bundled_codex_is_missing() {
        let root = TestDirectory::new("path-fallback");
        let bundled = root
            .path()
            .join("missing/ChatGPT.app/Contents/Resources/codex");
        let path_directory = root.path().join("path");
        fs::create_dir(&path_directory).unwrap();
        let path_codex = path_directory.join(executable_path(Path::new("codex")));
        write_executable(&path_codex);

        let resolved = resolve_from(
            Path::new("codex"),
            Some(path_variable(&path_directory).as_os_str()),
            Some(&bundled),
        )
        .unwrap();

        assert_eq!(resolved, fs::canonicalize(path_codex).unwrap());
    }

    #[test]
    fn keeps_an_explicit_codex_path_authoritative() {
        let root = TestDirectory::new("explicit");
        let explicit = root.path().join(executable_path(Path::new("custom-codex")));
        let bundled = root.path().join("bundled-codex");
        let path_directory = root.path().join("path");
        fs::create_dir(&path_directory).unwrap();
        write_executable(&explicit);
        write_executable(&bundled);
        write_executable(&path_directory.join("custom-codex"));

        let resolved = resolve_from(
            &explicit,
            Some(path_variable(&path_directory).as_os_str()),
            Some(&bundled),
        )
        .unwrap();

        assert_eq!(resolved, fs::canonicalize(explicit).unwrap());
    }

    #[test]
    fn reports_missing_bundled_and_path_candidates_without_invoking_codex() {
        let root = TestDirectory::new("missing");
        let bundled = root.path().join("missing-bundle/codex");
        let path_directory = root.path().join("missing-path");
        fs::create_dir(&path_directory).unwrap();

        let error = resolve_from(
            Path::new("codex"),
            Some(path_variable(&path_directory).as_os_str()),
            Some(&bundled),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            Error::ExecutableNotFound(path) if path == Path::new("codex")
        ));
    }
}
