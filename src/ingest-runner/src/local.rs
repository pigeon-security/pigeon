//! The local-folder source: every regular file under a root, by relative path.

use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use pigeon_ingest::Error;
use pigeon_ingest::engine::{Object, Page, Source};

/// Keys per page.
const PAGE: usize = 1000;

/// A folder as a source. Keys are relative paths joined with `/`, sorted bytewise. Symbolic
/// links are neither listed nor followed.
pub struct LocalSource {
    root: PathBuf,
    listing: OnceLock<Result<Vec<Object>, String>>,
}

impl LocalSource {
    /// A source over the folder at `root`.
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            listing: OnceLock::new(),
        }
    }

    fn walk(&self) -> Result<Vec<Object>, String> {
        let mut out = Vec::new();
        let mut stack = vec![(self.root.clone(), String::new())];
        while let Some((dir, prefix)) = stack.pop() {
            let read = std::fs::read_dir(&dir)
                .map_err(|e| format!("cannot read the folder {}: {e}", dir.display()))?;
            for entry in read {
                let entry = entry.map_err(|e| format!("cannot read a folder entry: {e}"))?;
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| format!("a file name in {} is not UTF-8", dir.display()))?;
                let kind = entry
                    .file_type()
                    .map_err(|e| format!("cannot stat {name}: {e}"))?;
                let key = format!("{prefix}{name}");
                if kind.is_dir() {
                    stack.push((entry.path(), format!("{key}/")));
                } else if kind.is_file() {
                    let size = entry
                        .metadata()
                        .map_err(|e| format!("cannot stat {key}: {e}"))?
                        .len();
                    out.push(Object {
                        key,
                        size,
                        etag: None,
                        modified: None,
                    });
                }
            }
        }
        out.sort_by(|a, b| a.key.as_bytes().cmp(b.key.as_bytes()));
        Ok(out)
    }

    /// The path of `key` under the root, refusing anything that climbs out or crosses a link.
    fn path_of(&self, key: &str) -> Result<PathBuf, Error> {
        let mut path = self.root.clone();
        for part in Path::new(key).components() {
            let Component::Normal(name) = part else {
                return Err(Error::Source(format!(
                    "the key {key} is not a relative path"
                )));
            };
            path.push(name);
            let meta = std::fs::symlink_metadata(&path)
                .map_err(|e| Error::Source(format!("cannot open {key}: {e}")))?;
            if meta.file_type().is_symlink() {
                return Err(Error::Source(format!("{key} goes through a symbolic link")));
            }
        }
        Ok(path)
    }
}

impl Source for LocalSource {
    fn list(&self, after: Option<&str>, token: Option<&str>) -> Result<Page, Error> {
        let all = self
            .listing
            .get_or_init(|| self.walk())
            .as_ref()
            .map_err(|e| Error::Source(e.clone()))?;
        let start = match token.or(after) {
            Some(k) => all.partition_point(|o| o.key.as_bytes() <= k.as_bytes()),
            None => 0,
        };
        let end = (start + PAGE).min(all.len());
        let objects = all[start..end].to_vec();
        let next = if end < all.len() {
            objects.last().map(|o| o.key.clone())
        } else {
            None
        };
        Ok(Page { objects, next })
    }

    fn open(&self, key: &str) -> Result<Box<dyn Read + Send>, Error> {
        let path = self.path_of(key)?;
        let file =
            File::open(&path).map_err(|e| Error::Source(format!("cannot open {key}: {e}")))?;
        Ok(Box::new(file))
    }
}
