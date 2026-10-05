use std::{
    env,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use crate::{
    Error,
    platform::{executable_path, is_executable},
};

pub(crate) const DEFAULT_CODEX_PROGRAM: &str = "codex";

pub(crate) fn resolve(program: &Path, path: Option<&OsStr>) -> Result<PathBuf, Error> {
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

    let path = path.ok_or_else(|| Error::ExecutableNotFound(program.to_path_buf()))?;
    for directory in env::split_paths(path) {
        let candidate = directory.join(executable_path(program));
        if let Some(resolved) = resolve_candidate(&candidate)? {
            return Ok(resolved);
        }
    }
    Err(Error::ExecutableNotFound(program.to_path_buf()))
}

fn resolve_candidate(candidate: &Path) -> Result<Option<PathBuf>, Error> {
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
    fn resolves_explicit_paths_and_uses_only_the_selected_instance_path() {
        let root = tempfile::tempdir().unwrap();
        let path_directory = root.path().join("path");
        let other_directory = root.path().join("other");
        let explicit = root.path().join(executable_path(Path::new("custom-codex")));
        fs::create_dir(&path_directory).unwrap();
        fs::create_dir(&other_directory).unwrap();
        let path_codex = path_directory.join(executable_path(Path::new("codex")));
        let other_codex = other_directory.join(executable_path(Path::new("codex")));
        let competing = path_directory.join(executable_path(Path::new("custom-codex")));
        for path in [&explicit, &path_codex, &other_codex, &competing] {
            write_executable(path);
        }
        let path = path_variable(&path_directory);
        assert_eq!(
            resolve(&explicit, Some(&path)).unwrap(),
            fs::canonicalize(&explicit).unwrap()
        );
        assert_eq!(
            resolve(Path::new("codex"), Some(&path)).unwrap(),
            fs::canonicalize(&path_codex).unwrap()
        );
        let other_path = path_variable(&other_directory);
        assert_eq!(
            resolve(Path::new("codex"), Some(&other_path)).unwrap(),
            fs::canonicalize(&other_codex).unwrap()
        );
        fs::remove_file(&path_codex).unwrap();
        assert!(
            matches!(resolve(Path::new("codex"), Some(&path)), Err(Error::ExecutableNotFound(path)) if path == Path::new("codex"))
        );
        assert!(matches!(
            resolve(Path::new("codex"), None),
            Err(Error::ExecutableNotFound(_))
        ));
    }
}
