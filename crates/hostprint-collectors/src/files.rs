//! Fingerprints of configured files: size, modification time, mode, SHA-256.
//! Contents are hashed, never stored.

use crate::redact::hex;
use crate::{CaptureContext, CollectError, Collected, Collector, Section};
use chrono::{DateTime, Utc};
use hostprint_model::FileFingerprint;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

/// Larger files are recorded without a hash.
const MAX_HASH_BYTES: u64 = 256 * 1024 * 1024;

pub struct FileCollector;

impl Collector for FileCollector {
    fn name(&self) -> &'static str {
        "files"
    }

    fn title(&self) -> &'static str {
        "Files"
    }

    fn collect(&self, ctx: &CaptureContext) -> Result<Collected, CollectError> {
        if ctx.file_paths.is_empty() {
            return Err(CollectError::Unavailable("no files configured ([files] paths in config.toml)".into()));
        }
        let mut files: Vec<FileFingerprint> = ctx.file_paths.iter().map(|p| fingerprint(p)).collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files.dedup_by(|a, b| a.path == b.path);
        let notes: Vec<String> =
            files.iter().filter_map(|f| f.error.as_ref().map(|e| format!("{}: {e}", f.path))).collect();
        let missing = files.iter().filter(|f| !f.exists).count();
        let summary = format!("{} files · {} missing", files.len(), missing);
        let mut collected = Collected::new(Section::Files(files)).summary(summary);
        collected.notes = notes;
        Ok(collected)
    }
}

pub(crate) fn fingerprint(path: &Path) -> FileFingerprint {
    let mut fp = FileFingerprint {
        path: path.display().to_string(),
        exists: false,
        size: None,
        modified: None,
        sha256: None,
        mode: None,
        uid: None,
        gid: None,
        error: None,
    };
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return fp,
        Err(e) => {
            // Exists, as far as we can tell, but we may not look.
            fp.exists = true;
            fp.error = Some(e.to_string());
            return fp;
        }
    };
    fp.exists = true;
    fp.size = Some(meta.len());
    fp.modified = meta.modified().ok().map(DateTime::<Utc>::from);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fp.mode = Some(format!("{:04o}", meta.mode() & 0o7777));
        fp.uid = Some(meta.uid());
        fp.gid = Some(meta.gid());
    }
    if meta.is_file() {
        if meta.len() > MAX_HASH_BYTES {
            fp.error = Some(format!("larger than {} MiB; not hashed", MAX_HASH_BYTES / 1024 / 1024));
        } else {
            match hash_file(path) {
                Ok(hash) => fp.sha256 = Some(hash),
                Err(e) => fp.error = Some(e.to_string()),
            }
        }
    }
    fp
}

fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_files() {
        let dir = std::env::temp_dir().join(format!("hostprint-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("nginx.conf");
        std::fs::write(&file, "abc").unwrap();
        let fp = fingerprint(&file);
        assert!(fp.exists);
        assert_eq!(fp.size, Some(3));
        assert_eq!(fp.sha256.as_deref(), Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
        let missing = fingerprint(&dir.join("missing.conf"));
        assert!(!missing.exists);
        assert_eq!(missing.sha256, None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
