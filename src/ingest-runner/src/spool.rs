//! The spool: files in one directory, named by a counter, removed when dropped.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pigeon_ingest::Error;
use pigeon_ingest::engine::{Spool, SpoolFile};

/// A spool in a directory with a byte budget. File names are a counter, never a source name.
pub struct DirSpool {
    dir: PathBuf,
    budget: u64,
    counter: AtomicU64,
    held: Arc<AtomicU64>,
}

impl DirSpool {
    /// Uses `dir` (made if missing) with room for `budget` bytes.
    pub fn new(dir: PathBuf, budget: u64) -> Result<Self, Error> {
        std::fs::create_dir_all(&dir)
            .map_err(|e| Error::Io(format!("cannot make the spool directory: {e}")))?;
        Ok(Self {
            dir,
            budget,
            counter: AtomicU64::new(0),
            held: Arc::new(AtomicU64::new(0)),
        })
    }

    /// The spool directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Spool for DirSpool {
    fn create(&self) -> Result<Box<dyn SpoolFile>, Error> {
        let n = self.counter.fetch_add(1, Ordering::SeqCst);
        let path = self.dir.join(format!("spool-{n}.tmp"));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| Error::Io(format!("cannot make a spool file: {e}")))?;
        Ok(Box::new(Handle {
            file,
            path,
            held: Arc::clone(&self.held),
            pos: 0,
            len: 0,
        }))
    }

    fn free_bytes(&self) -> u64 {
        self.budget.saturating_sub(self.held.load(Ordering::SeqCst))
    }
}

/// One spool file. Counts its length against the spool and deletes itself on drop.
struct Handle {
    file: File,
    path: PathBuf,
    held: Arc<AtomicU64>,
    pos: u64,
    len: u64,
}

impl Read for Handle {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.file.read(buf)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Write for Handle {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.file.write(buf)?;
        self.pos += n as u64;
        if self.pos > self.len {
            self.held.fetch_add(self.pos - self.len, Ordering::SeqCst);
            self.len = self.pos;
        }
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

impl Seek for Handle {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.pos = self.file.seek(to)?;
        Ok(self.pos)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.held.fetch_sub(self.len, Ordering::SeqCst);
        let _ = std::fs::remove_file(&self.path);
    }
}
