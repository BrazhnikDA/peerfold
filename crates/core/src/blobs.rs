use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::model::valid_hash;

#[derive(Clone)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn path(&self, hash: &str) -> Result<PathBuf> {
        if !valid_hash(hash) {
            return Err(Error::msg("плохой хеш"));
        }
        Ok(self.root.join(&hash[..2]).join(hash))
    }

    pub fn ingest_file(&self, source: &Path) -> Result<(String, u64)> {
        let file = File::open(source)?;
        self.ingest_reader(file)
    }

    pub fn ingest_reader<R: Read>(&self, mut reader: R) -> Result<(String, u64)> {
        let tmp = self.root.join(format!("tmp-{}-{}.part", std::process::id(), crate::model::now_ns()));
        let mut out = File::create(&tmp)?;
        let mut hasher = blake3::Hasher::new();
        let mut buf = vec![0u8; 256 * 1024];
        let mut size = 0u64;
        loop {
            let read = reader.read(&mut buf)?;
            if read == 0 {
                break;
            }
            hasher.update(&buf[..read]);
            out.write_all(&buf[..read])?;
            size += read as u64;
        }
        out.flush()?;
        drop(out);
        let hash = hasher.finalize().to_hex().to_string();
        let dest = self.path(&hash)?;
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        if dest.exists() {
            let _ = fs::remove_file(&tmp);
        } else if fs::rename(&tmp, &dest).is_err() {
            if !dest.exists() {
                fs::copy(&tmp, &dest)?;
            }
            let _ = fs::remove_file(&tmp);
        }
        Ok((hash, size))
    }

    pub fn remove_if_unused(&self, hash: &str) -> Result<()> {
        if !valid_hash(hash) {
            return Ok(());
        }
        if let Ok(path) = self.path(hash) {
            let _ = fs::remove_file(path);
        }
        Ok(())
    }
}
