//! Local snapshot storage under `~/.hostprint` (or `$HOSTPRINT_HOME`).
//!
//! ```text
//! ~/.hostprint/
//! ├── config.toml
//! ├── fingerprint.key      secret-fingerprint key, 0600
//! ├── snapshots/
//! │   ├── healthy.hp       snapshot JSON, 0600
//! │   └── incident.hp
//! └── baselines/
//!     └── production.hp    known-good state for `hostprint check`
//! ```
//!
//! Snapshots can contain sensitive operational detail even after redaction,
//! so directories are created `0700` and files `0600`.

use chrono::{DateTime, Utc};
use hostprint_model::{Snapshot, SCHEMA_VERSION};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const SNAPSHOT_EXTENSION: &str = "hp";
/// Extension of exported snapshots meant to be shared.
pub const EXPORT_EXTENSION: &str = "hostprint";
const KEY_LEN: usize = 32;
const MAX_NAME_LEN: usize = 64;

/// The two kinds of stored snapshot. They share a format and differ only in
/// where they live and how they are used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Snapshot,
    /// A known-good state that `hostprint check` compares against.
    Baseline,
}

impl Kind {
    fn dir_name(self) -> &'static str {
        match self {
            Kind::Snapshot => "snapshots",
            Kind::Baseline => "baselines",
        }
    }

    fn list_command(self) -> &'static str {
        match self {
            Kind::Snapshot => "hostprint list",
            Kind::Baseline => "hostprint baseline list",
        }
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Kind::Snapshot => "snapshot",
            Kind::Baseline => "baseline",
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("no {kind} named '{name}' (see `{}`)", .kind.list_command())]
    NotFound { kind: Kind, name: String },
    #[error("a {kind} named '{name}' already exists (use --force to replace it)")]
    AlreadyExists { kind: Kind, name: String },
    #[error("invalid snapshot name '{0}': use 1-64 letters, digits, '.', '_' or '-', not starting with '.' or '-'")]
    InvalidName(String),
    #[error("{}: written by a newer Hostprint (schema {}; this version reads up to {})", .path.display(), .found, .supported)]
    UnsupportedSchema { path: PathBuf, found: u64, supported: u32 },
    #[error("{}: not a Hostprint snapshot ({})", .path.display(), .reason)]
    Invalid { path: PathBuf, reason: String },
    #[error("{}: {}", .path.display(), .source)]
    Io { path: PathBuf, source: io::Error },
    #[error("cannot determine home directory; set HOSTPRINT_HOME")]
    NoHome,
}

pub type Result<T> = std::result::Result<T, StorageError>;

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> StorageError + '_ {
    move |source| StorageError::Io { path: path.to_path_buf(), source }
}

/// What `hostprint list` shows about a stored snapshot.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
    /// The snapshot's metadata, or why it could not be read.
    pub info: std::result::Result<EntryInfo, String>,
}

#[derive(Debug, Clone)]
pub struct EntryInfo {
    pub id: String,
    pub captured_at: DateTime<Utc>,
    pub hostname: Option<String>,
    pub collectors_with_data: usize,
    pub collectors_total: usize,
}

#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// `$HOSTPRINT_HOME`, else `~/.hostprint`.
    pub fn default_root() -> Result<PathBuf> {
        if let Some(home) = std::env::var_os("HOSTPRINT_HOME").filter(|h| !h.is_empty()) {
            return Ok(PathBuf::from(home));
        }
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .filter(|h| !h.is_empty())
            .map(|h| PathBuf::from(h).join(".hostprint"))
            .ok_or(StorageError::NoHome)
    }

    pub fn new(root: impl Into<PathBuf>) -> Store {
        Store { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn snapshots_dir(&self) -> PathBuf {
        self.dir(Kind::Snapshot)
    }

    pub fn dir(&self, kind: Kind) -> PathBuf {
        self.root.join(kind.dir_name())
    }

    pub fn config_path(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    pub fn key_path(&self) -> PathBuf {
        self.root.join("fingerprint.key")
    }

    pub fn snapshot_path(&self, name: &str) -> PathBuf {
        self.path_of(Kind::Snapshot, name)
    }

    pub fn path_of(&self, kind: Kind, name: &str) -> PathBuf {
        self.dir(kind).join(format!("{name}.{SNAPSHOT_EXTENSION}"))
    }

    /// Creates the storage directories with private permissions.
    pub fn init(&self) -> Result<()> {
        create_private_dir(&self.root)?;
        create_private_dir(&self.snapshots_dir())
    }

    pub fn exists(&self, name: &str) -> bool {
        self.exists_in(Kind::Snapshot, name)
    }

    pub fn exists_in(&self, kind: Kind, name: &str) -> bool {
        self.path_of(kind, name).exists()
    }

    /// Writes a snapshot atomically. Returns the file path.
    pub fn save(&self, snapshot: &Snapshot, overwrite: bool) -> Result<PathBuf> {
        self.save_in(Kind::Snapshot, snapshot, overwrite)
    }

    /// Writes a snapshot of the given kind, named after `snapshot.name`.
    pub fn save_in(&self, kind: Kind, snapshot: &Snapshot, overwrite: bool) -> Result<PathBuf> {
        validate_name(&snapshot.name)?;
        self.init()?;
        create_private_dir(&self.dir(kind))?;
        let path = self.path_of(kind, &snapshot.name);
        if path.exists() && !overwrite {
            return Err(StorageError::AlreadyExists { kind, name: snapshot.name.clone() });
        }
        write_private(&path, &to_json(snapshot))?;
        Ok(path)
    }

    pub fn load(&self, name: &str) -> Result<Snapshot> {
        self.load_from(Kind::Snapshot, name)
    }

    pub fn load_from(&self, kind: Kind, name: &str) -> Result<Snapshot> {
        validate_name(name)?;
        let path = self.path_of(kind, name);
        if !path.exists() {
            return Err(StorageError::NotFound { kind, name: name.to_string() });
        }
        read_snapshot_file(&path)
    }

    /// Loads by name, or from a file if `reference` is a path to one.
    pub fn resolve(&self, reference: &str) -> Result<Snapshot> {
        let looks_like_path = reference.contains('/')
            || reference.contains('\\')
            || [".json", ".hp", ".hostprint"].iter().any(|ext| reference.ends_with(ext));
        if looks_like_path && Path::new(reference).is_file() {
            return read_snapshot_file(Path::new(reference));
        }
        self.load(reference)
    }

    pub fn delete(&self, name: &str) -> Result<PathBuf> {
        self.delete_from(Kind::Snapshot, name)
    }

    pub fn delete_from(&self, kind: Kind, name: &str) -> Result<PathBuf> {
        validate_name(name)?;
        let path = self.path_of(kind, name);
        match fs::remove_file(&path) {
            Ok(()) => Ok(path),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Err(StorageError::NotFound { kind, name: name.to_string() })
            }
            Err(e) => Err(io_err(&path)(e)),
        }
    }

    /// Writes a snapshot to `path` for sharing, with private permissions.
    /// Refuses to overwrite an existing file unless `overwrite` is set.
    pub fn export(&self, snapshot: &Snapshot, path: &Path, overwrite: bool) -> Result<()> {
        if path.exists() && !overwrite {
            return Err(StorageError::Io {
                path: path.to_path_buf(),
                source: io::Error::new(io::ErrorKind::AlreadyExists, "file exists (use --force to replace it)"),
            });
        }
        write_private(path, &to_json(snapshot))
    }

    /// Stored snapshots, oldest first.
    pub fn list(&self) -> Result<Vec<Entry>> {
        self.list_in(Kind::Snapshot)
    }

    /// Stored snapshots of one kind, oldest first.
    pub fn list_in(&self, kind: Kind) -> Result<Vec<Entry>> {
        let dir = self.dir(kind);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_err(&dir)(e)),
        };
        let mut out: Vec<Entry> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let path = e.path();
                if path.extension()? != SNAPSHOT_EXTENSION {
                    return None;
                }
                let name = path.file_stem()?.to_str()?.to_string();
                let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                let info = read_snapshot_file(&path)
                    .map(|s| EntryInfo {
                        id: s.id.clone(),
                        captured_at: s.captured_at,
                        hostname: s.hostname().map(str::to_string),
                        collectors_with_data: s.capture.collectors.iter().filter(|c| c.status.has_data()).count(),
                        collectors_total: s.capture.collectors.len(),
                    })
                    .map_err(|e| e.to_string());
                Some(Entry { name, path, size, info })
            })
            .collect();
        out.sort_by(|a, b| {
            let time = |e: &Entry| e.info.as_ref().ok().map(|i| i.captured_at);
            time(a).cmp(&time(b)).then_with(|| a.name.cmp(&b.name))
        });
        Ok(out)
    }

    /// The per-installation key used to fingerprint secrets, created on first use.
    pub fn fingerprint_key(&self) -> Result<Vec<u8>> {
        let path = self.key_path();
        match fs::read(&path) {
            Ok(key) if key.len() >= 16 => return Ok(key),
            Ok(_) => {
                return Err(StorageError::Invalid { path, reason: "fingerprint key is truncated".into() });
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(&path)(e)),
        }
        self.init()?;
        let mut key = vec![0u8; KEY_LEN];
        getrandom::fill(&mut key).map_err(|e| StorageError::Io {
            path: path.clone(),
            source: io::Error::other(format!("no system randomness: {e}")),
        })?;
        write_private(&path, &key)?;
        Ok(key)
    }
}

pub fn validate_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && !name.starts_with(['.', '-'])
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if valid {
        Ok(())
    } else {
        Err(StorageError::InvalidName(name.to_string()))
    }
}

fn to_json(snapshot: &Snapshot) -> Vec<u8> {
    let mut json = serde_json::to_vec_pretty(snapshot).expect("snapshots always serialize");
    json.push(b'\n');
    json
}

/// Reads a snapshot file, refusing schema versions newer than this build.
pub fn read_snapshot_file(path: &Path) -> Result<Snapshot> {
    let bytes = fs::read(path).map_err(io_err(path))?;
    parse_snapshot(&bytes).map_err(|e| match e {
        ParseError::Schema(found) => {
            StorageError::UnsupportedSchema { path: path.to_path_buf(), found, supported: SCHEMA_VERSION }
        }
        ParseError::Invalid(reason) => StorageError::Invalid { path: path.to_path_buf(), reason },
    })
}

enum ParseError {
    Schema(u64),
    Invalid(String),
}

fn parse_snapshot(bytes: &[u8]) -> std::result::Result<Snapshot, ParseError> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| ParseError::Invalid(e.to_string()))?;
    let version = value
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| ParseError::Invalid("missing schemaVersion".into()))?;
    if version > u64::from(SCHEMA_VERSION) {
        return Err(ParseError::Schema(version));
    }
    serde_json::from_value(value).map_err(|e| ParseError::Invalid(e.to_string()))
}

fn create_private_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        match fs::DirBuilder::new().recursive(true).mode(0o700).create(path) {
            Ok(()) => Ok(()),
            Err(e) => Err(io_err(path)(e)),
        }
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path).map_err(io_err(path))
    }
}

/// Writes via a temporary file and rename, with `0600` permissions.
fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = path.with_file_name(format!(".{file_name}.tmp-{}", std::process::id()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(io_err(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hostprint_model::CaptureInfo;

    fn temp_store(tag: &str) -> Store {
        let root = std::env::temp_dir().join(format!("hostprint-store-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        Store::new(root)
    }

    fn snapshot(name: &str) -> Snapshot {
        Snapshot {
            schema_version: SCHEMA_VERSION,
            id: "snap_test".into(),
            name: name.into(),
            captured_at: DateTime::from_timestamp(1_790_000_000, 0).unwrap(),
            capture: CaptureInfo {
                hostprint_version: "0.1.0".into(),
                duration_ms: 1,
                user: None,
                uid: None,
                elevated: false,
                working_dir: None,
                remote: None,
                collectors: Vec::new(),
            },
            host: None,
            resources: None,
            processes: None,
            network: None,
            services: None,
            docker: None,
            git: None,
            runtimes: None,
            environment: None,
            files: None,
            logs: None,
        }
    }

    #[test]
    fn saves_loads_lists_and_deletes() {
        let store = temp_store("crud");
        let path = store.save(&snapshot("healthy"), false).unwrap();
        assert_eq!(store.load("healthy").unwrap(), snapshot("healthy"));
        assert_eq!(store.resolve(path.to_str().unwrap()).unwrap().name, "healthy");
        assert!(matches!(
            store.save(&snapshot("healthy"), false),
            Err(StorageError::AlreadyExists { kind: Kind::Snapshot, .. })
        ));
        store.save(&snapshot("healthy"), true).unwrap();
        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "healthy");
        store.delete("healthy").unwrap();
        let err = store.load("healthy").unwrap_err();
        assert!(matches!(err, StorageError::NotFound { .. }));
        assert_eq!(err.to_string(), "no snapshot named 'healthy' (see `hostprint list`)");
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn baselines_are_kept_apart_from_snapshots() {
        let store = temp_store("baselines");
        store.save_in(Kind::Baseline, &snapshot("production"), false).unwrap();
        assert!(store.exists_in(Kind::Baseline, "production"));
        assert!(!store.exists("production"));
        assert_eq!(store.list().unwrap().len(), 0);
        assert_eq!(store.list_in(Kind::Baseline).unwrap()[0].name, "production");
        assert_eq!(store.load_from(Kind::Baseline, "production").unwrap().name, "production");
        let err = store.load_from(Kind::Baseline, "staging").unwrap_err();
        assert_eq!(err.to_string(), "no baseline named 'staging' (see `hostprint baseline list`)");
        store.delete_from(Kind::Baseline, "production").unwrap();
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn exports_refuse_to_overwrite() {
        let store = temp_store("export");
        let out = store.root().join("out.hostprint");
        fs::create_dir_all(store.root()).unwrap();
        store.export(&snapshot("s"), &out, false).unwrap();
        assert_eq!(store.resolve(out.to_str().unwrap()).unwrap().name, "s");
        assert!(store.export(&snapshot("s"), &out, false).is_err());
        store.export(&snapshot("s"), &out, true).unwrap();
        let _ = fs::remove_dir_all(store.root());
    }

    #[cfg(unix)]
    #[test]
    fn uses_private_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let store = temp_store("perms");
        let path = store.save(&snapshot("s"), false).unwrap();
        store.fingerprint_key().unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&store.key_path()), 0o600);
        assert_eq!(mode(&store.snapshots_dir()), 0o700);
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn fingerprint_key_is_stable() {
        let store = temp_store("key");
        let a = store.fingerprint_key().unwrap();
        assert_eq!(a.len(), KEY_LEN);
        assert_eq!(store.fingerprint_key().unwrap(), a);
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn rejects_bad_names_and_newer_schemas() {
        for bad in ["", "../etc", ".hidden", "-flag", "a b", &"x".repeat(65)] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
        validate_name("incident-2026.10.01_b").unwrap();

        let store = temp_store("schema");
        store.init().unwrap();
        let path = store.snapshot_path("future");
        fs::write(&path, r#"{"schemaVersion": 99}"#).unwrap();
        assert!(matches!(store.load("future"), Err(StorageError::UnsupportedSchema { found: 99, .. })));
        fs::write(&path, r#"{"hello": 1}"#).unwrap();
        assert!(matches!(store.load("future"), Err(StorageError::Invalid { .. })));
        let _ = fs::remove_dir_all(store.root());
    }
}
