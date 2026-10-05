use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Other,
}

/// What a `stat` (following symlinks) reports about a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStat {
    pub kind: EntryKind,
    pub size: u64,
    pub mtime_ms: Option<i64>,
    pub device: u64,
    pub inode: Option<u64>,
    pub birthtime_ms: Option<i64>,
}

pub trait TranscriptFile: Send {
    /// Up to `max` bytes, or `None` at end of file.
    fn read(&mut self, max: usize) -> io::Result<Option<Vec<u8>>>;
    fn stat(&self) -> io::Result<FileStat>;
}

/// The read-only file system the scanner uses; tests substitute faults and counters.
pub trait TranscriptFs: Send + Sync {
    fn read_dir(&self, dir: &Path) -> io::Result<Vec<String>>;
    fn stat(&self, path: &Path) -> io::Result<FileStat>;
    fn real_path(&self, path: &Path) -> io::Result<PathBuf>;
    fn open(&self, path: &Path) -> io::Result<Box<dyn TranscriptFile>>;
    fn read_to_string(&self, path: &Path) -> io::Result<String>;
}

pub struct OsFs;

impl TranscriptFs for OsFs {
    fn read_dir(&self, dir: &Path) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            if let Ok(name) = entry?.file_name().into_string() {
                names.push(name);
            }
        }
        Ok(names)
    }
    fn stat(&self, path: &Path) -> io::Result<FileStat> {
        Ok(file_stat(&std::fs::metadata(path)?))
    }
    fn real_path(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }
    fn open(&self, path: &Path) -> io::Result<Box<dyn TranscriptFile>> {
        Ok(Box::new(OsFile(std::fs::File::open(path)?)))
    }
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }
}

struct OsFile(std::fs::File);

impl TranscriptFile for OsFile {
    fn read(&mut self, max: usize) -> io::Result<Option<Vec<u8>>> {
        let mut buffer = vec![0; max];
        let read = loop {
            match self.0.read(&mut buffer) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => break result?,
            }
        };
        if read == 0 {
            return Ok(None);
        }
        buffer.truncate(read);
        Ok(Some(buffer))
    }
    fn stat(&self) -> io::Result<FileStat> {
        Ok(file_stat(&self.0.metadata()?))
    }
}

fn millis(time: io::Result<SystemTime>) -> Option<i64> {
    let time = time.ok()?;
    Some(match time.duration_since(UNIX_EPOCH) {
        Ok(after) => after.as_millis() as i64,
        Err(before) => -(before.duration().as_millis() as i64),
    })
}

pub fn file_stat(metadata: &std::fs::Metadata) -> FileStat {
    #[cfg(unix)]
    let (device, inode) = {
        use std::os::unix::fs::MetadataExt;
        (metadata.dev(), Some(metadata.ino()))
    };
    #[cfg(not(unix))]
    let (device, inode) = (0, None);
    FileStat {
        kind: if metadata.is_file() {
            EntryKind::File
        } else if metadata.is_dir() {
            EntryKind::Directory
        } else {
            EntryKind::Other
        },
        size: metadata.len(),
        mtime_ms: millis(metadata.modified()),
        device,
        inode,
        birthtime_ms: millis(metadata.created()),
    }
}
