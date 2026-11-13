use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs, io,
    io::Write,
    path::Component,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
    sync::{Arc, Mutex, OnceLock},
};

const META_DIR: &str = ".brr-fleet";
const REPO_DIR: &str = "repo";
const ORIGIN_DIR: &str = "origin.git";
const SCHEMA_VERSION: u32 = 1;
const MAX_VALIDATED_TREE_CACHE_ENTRIES: usize = 8;
const MAX_VALIDATED_TREE_CACHE_BYTES: usize = 64 * 1024 * 1024;
const MAX_VALIDATED_TREE_ENTRY_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    Blob,
    Agent,
    Memory,
    Skill,
    Project,
    Transcript,
    Task,
    Workflow,
}

impl RecordKind {
    fn path_segment(&self) -> &'static str {
        match self {
            Self::Blob => "blob",
            Self::Agent => "agent",
            Self::Memory => "memory",
            Self::Skill => "skill",
            Self::Project => "project",
            Self::Transcript => "transcript",
            Self::Task => "task",
            Self::Workflow => "workflow",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub schema_version: u32,
    pub id: String,
    pub kind: RecordKind,
    pub origin_node_id: String,
    pub revision: String,
    pub base_revision: Option<String>,
    pub deleted: bool,
    pub content: Value,
}

/// Verified source for an immutable content-addressed attachment. The source
/// is copied into the repository with O_NOFOLLOW and rehashed as part of the
/// same commit that publishes its metadata record.
#[derive(Clone, Debug)]
pub struct BlobFile {
    pub sha256: String,
    pub size_bytes: u64,
    pub source_path: PathBuf,
}

impl Record {
    pub fn entity_id(&self) -> EntityId {
        EntityId {
            origin_node_id: self.origin_node_id.clone(),
            id: self.id.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityId {
    pub origin_node_id: String,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Conflict {
    pub entity_id: EntityId,
    pub id: String,
    pub base: Option<Record>,
    pub current: Option<Record>,
    pub incoming: Option<Record>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncOutcome {
    UpToDate,
    Published { branch: String },
    Merged { commit: String, records: usize },
    Conflicted { records: Vec<String> },
}

#[derive(Debug)]
pub enum FleetError {
    Io(io::Error),
    Git {
        args: Vec<String>,
        stderr: String,
    },
    Invalid(String),
    RevisionConflict {
        id: String,
        expected: Option<String>,
        actual: Option<String>,
    },
    Conflict(String),
    Transfer(String),
    Serialization(serde_json::Error),
}

impl std::fmt::Display for FleetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "filesystem error: {error}"),
            Self::Git { args, stderr } => {
                write!(f, "git {} failed: {}", args.join(" "), stderr.trim())
            }
            Self::Invalid(message) => write!(f, "invalid fleet data: {message}"),
            Self::RevisionConflict {
                id,
                expected,
                actual,
            } => write!(
                f,
                "revision conflict for {id}: expected {expected:?}, found {actual:?}"
            ),
            Self::Conflict(message) => write!(f, "fleet conflict: {message}"),
            Self::Transfer(message) => write!(f, "fleet Git transfer failed: {message}"),
            Self::Serialization(error) => write!(f, "record serialization error: {error}"),
        }
    }
}

impl std::error::Error for FleetError {}
impl From<io::Error> for FleetError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}
impl From<serde_json::Error> for FleetError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serialization(value)
    }
}
pub type Result<T> = std::result::Result<T, FleetError>;

type DomainValidator = Arc<dyn Fn(&[Record]) -> std::result::Result<(), String> + Send + Sync>;

/// Performs remote origin transfers through a caller-owned, policy-pinned transport.
/// Implementations receive the local repository path; branch names are logical names
/// such as `main` or `nodes/<node-id>`.
pub trait GitTransfer: Send + Sync {
    fn fetch(&self, repo: &Path) -> std::result::Result<(), String>;
    fn push(&self, repo: &Path, branch: &str) -> std::result::Result<(), String>;
}

#[derive(Clone)]
pub struct FleetRepo {
    data_root: PathBuf,
    meta_root: PathBuf,
    repo: PathBuf,
    origin: String,
    node_id: String,
    primary: bool,
    domain_validator: Option<DomainValidator>,
    git_transfer: Option<Arc<dyn GitTransfer>>,
}

impl FleetRepo {
    pub fn init_primary(root: impl AsRef<Path>, node_id: impl Into<String>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let node_id = node_id.into();
        validate_node_id(&node_id)?;
        let meta_root = root.join(META_DIR);
        let repo = meta_root.join(REPO_DIR);
        let bare_origin = meta_root.join(ORIGIN_DIR);
        fs::create_dir_all(&meta_root)?;
        fs::create_dir_all(meta_root.join("no-hooks"))?;
        // Initialization is resumable after any individual Git command. Do
        // not delete a partial repository: validate any existing identity and
        // finish only missing configuration, refs, or the initial push.
        if !bare_origin.join("HEAD").exists() {
            git(
                &root,
                [
                    "init",
                    "--bare",
                    bare_origin
                        .to_str()
                        .ok_or_else(|| FleetError::Invalid("non-UTF8 metadata path".into()))?,
                ],
            )?;
        } else if git_config(&bare_origin, "core.bare")? != "true" {
            return Err(FleetError::Invalid(
                "primary origin path is not a bare repository".into(),
            ));
        }
        git(
            &bare_origin,
            [
                "config",
                "core.hooksPath",
                meta_root
                    .join("no-hooks")
                    .to_str()
                    .ok_or_else(|| FleetError::Invalid("non-UTF8 hooks path".into()))?,
            ],
        )?;
        fs::create_dir_all(&repo)?;
        if !repo.join(".git").exists() {
            git(&repo, ["init", "--initial-branch=main"])?;
        }
        ensure_fleet_identity(&repo, &node_id, true, bare_origin.to_str().unwrap())?;
        configure_repo(&repo, &node_id, &meta_root)?;
        ensure_origin_remote(&repo, bare_origin.to_str().unwrap())?;
        fs::create_dir_all(repo.join("records"))?;
        let has_head = git(&repo, ["rev-parse", "--verify", "HEAD"]).is_ok();
        let keep_path = repo.join("records/.keep");
        if has_head {
            let committed_keep = git(&repo, ["show", "HEAD:records/.keep"])?;
            if committed_keep.stdout != b"Fleet records are stored as canonical JSON files.\n" {
                return Err(FleetError::Invalid(
                    "existing primary repository does not contain the expected initialization file"
                        .into(),
                ));
            }
        }
        if keep_path.exists() {
            if fs::read(&keep_path)? != b"Fleet records are stored as canonical JSON files.\n" {
                return Err(FleetError::Invalid(
                    "existing primary initialization file has unexpected content".into(),
                ));
            }
        } else {
            fs::write(
                &keep_path,
                b"Fleet records are stored as canonical JSON files.\n",
            )?;
        }
        if !has_head {
            let staged = staged_paths(&repo)?;
            if staged.iter().any(|path| path != "records/.keep") {
                return Err(FleetError::Invalid(
                    "unexpected staged files in interrupted primary initialization".into(),
                ));
            }
            if staged.iter().any(|path| path == "records/.keep") {
                let staged_keep = git(&repo, ["show", ":records/.keep"])?;
                if staged_keep.stdout != b"Fleet records are stored as canonical JSON files.\n" {
                    return Err(FleetError::Invalid(
                        "staged primary initialization file has unexpected content".into(),
                    ));
                }
            }
            git(&repo, ["add", "records/.keep"])?;
            git(
                &repo,
                ["commit", "-m", "Initialize private fleet repository"],
            )?;
        }
        ensure_branch(&repo, "main")?;
        if git(&bare_origin, ["rev-parse", "--verify", "refs/heads/main"]).is_err() {
            git(&repo, ["push", "-u", "origin", "main"])?;
        } else {
            let local = git(&repo, ["rev-parse", "HEAD"])?;
            let remote = git(&bare_origin, ["rev-parse", "refs/heads/main"])?;
            if String::from_utf8_lossy(&local.stdout).trim()
                != String::from_utf8_lossy(&remote.stdout).trim()
            {
                return Err(FleetError::Invalid(
                    "primary origin main has an unexpected commit".into(),
                ));
            }
        }
        clear_validated_tree_cache(&repo)?;
        Ok(Self {
            data_root: root,
            meta_root,
            repo,
            origin: bare_origin.to_string_lossy().into_owned(),
            node_id,
            primary: true,
            domain_validator: None,
            git_transfer: None,
        })
    }

    pub fn init_node(
        root: impl AsRef<Path>,
        node_id: impl Into<String>,
        origin: &str,
    ) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let node_id = node_id.into();
        validate_node_id(&node_id)?;
        if origin.trim().is_empty() {
            return Err(FleetError::Invalid("origin must not be empty".into()));
        }
        let meta_root = root.join(META_DIR);
        let repo = meta_root.join(REPO_DIR);
        fs::create_dir_all(&meta_root)?;
        fs::create_dir_all(meta_root.join("no-hooks"))?;
        fs::create_dir_all(&meta_root)?;
        fs::create_dir_all(meta_root.join("no-hooks"))?;
        if !repo.join(".git").exists() {
            fs::create_dir_all(&repo)?;
            git(&repo, ["init", "--initial-branch=main"])?;
        }
        ensure_fleet_identity(&repo, &node_id, false, origin)?;
        configure_repo(&repo, &node_id, &meta_root)?;
        ensure_origin_remote(&repo, origin)?;
        git(&repo, ["fetch", "origin"])?;
        ensure_node_branch(&repo, &node_id)?;
        ensure_node_push(&repo, &node_id, None)?;
        clear_validated_tree_cache(&repo)?;
        Ok(Self {
            data_root: root,
            meta_root,
            repo,
            origin: origin.to_owned(),
            node_id,
            primary: false,
            domain_validator: None,
            git_transfer: None,
        })
    }

    /// Initialize a node checkout using an injected transport for every remote
    /// fetch and push. Local Git operations continue to use the normal Git client.
    pub fn init_node_with_transport(
        root: impl AsRef<Path>,
        node_id: impl Into<String>,
        origin: &str,
        transport: Arc<dyn GitTransfer>,
    ) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let node_id = node_id.into();
        validate_node_id(&node_id)?;
        if origin.trim().is_empty() {
            return Err(FleetError::Invalid("origin must not be empty".into()));
        }
        let meta_root = root.join(META_DIR);
        let repo = meta_root.join(REPO_DIR);
        fs::create_dir_all(&meta_root)?;
        fs::create_dir_all(meta_root.join("no-hooks"))?;
        fs::create_dir_all(&repo)?;
        if !repo.join(".git").exists() {
            git(&repo, ["init", "--initial-branch=main"])?;
        }
        ensure_fleet_identity(&repo, &node_id, false, origin)?;
        configure_repo(&repo, &node_id, &meta_root)?;
        ensure_origin_remote(&repo, origin)?;
        transport.fetch(&repo).map_err(FleetError::Transfer)?;
        ensure_node_branch(&repo, &node_id)?;
        ensure_node_push(&repo, &node_id, Some(&transport))?;
        clear_validated_tree_cache(&repo)?;
        Ok(Self {
            data_root: root,
            meta_root,
            repo,
            origin: origin.to_owned(),
            node_id,
            primary: false,
            domain_validator: None,
            git_transfer: Some(transport),
        })
    }

    /// Reopen an existing metadata checkout after process restart. Persisted
    /// node identity, role, origin and branch are verified before use.
    pub fn open(root: impl AsRef<Path>, node_id: impl Into<String>) -> Result<Self> {
        let node_id = node_id.into();
        validate_node_id(&node_id)?;
        let root = root.as_ref().to_path_buf();
        let meta_root = root.join(META_DIR);
        let repo = meta_root.join(REPO_DIR);
        if !repo.join(".git").exists() {
            return Err(FleetError::Invalid(
                "fleet repository is not initialized".into(),
            ));
        }
        let stored_node = git_config(&repo, "fleet.nodeId")?;
        if stored_node != node_id {
            return Err(FleetError::Invalid(format!(
                "fleet node identity mismatch: expected {node_id}, found {stored_node}"
            )));
        }
        let primary = match git_config(&repo, "fleet.primary")?.as_str() {
            "true" => true,
            "false" => false,
            _ => {
                return Err(FleetError::Invalid(
                    "missing or invalid persisted fleet role".into(),
                ));
            }
        };
        let origin = git_config(&repo, "fleet.origin")?;
        let remote = git_config(&repo, "remote.origin.url")?;
        if remote != origin {
            return Err(FleetError::Invalid(
                "persisted fleet origin does not match Git remote origin".into(),
            ));
        }
        let current_branch = git(&repo, ["branch", "--show-current"])?;
        let current_branch = String::from_utf8_lossy(&current_branch.stdout)
            .trim()
            .to_owned();
        let expected_branch = if primary {
            "main".to_owned()
        } else {
            format!("nodes/{node_id}")
        };
        if current_branch != expected_branch {
            return Err(FleetError::Invalid(format!(
                "fleet branch mismatch: expected {expected_branch}, found {current_branch}"
            )));
        }
        let bare_origin = meta_root.join(ORIGIN_DIR);
        if primary {
            let expected = bare_origin.to_string_lossy().into_owned();
            if origin != expected || !bare_origin.join("HEAD").exists() {
                return Err(FleetError::Invalid(
                    "primary fleet origin identity is invalid".into(),
                ));
            }
        } else if origin.trim().is_empty() {
            return Err(FleetError::Invalid("node fleet origin is empty".into()));
        }
        clear_validated_tree_cache(&repo)?;
        Ok(Self {
            data_root: root,
            meta_root,
            repo,
            origin,
            node_id,
            primary,
            domain_validator: None,
            git_transfer: None,
        })
    }

    pub fn open_with_transport(
        root: impl AsRef<Path>,
        node_id: impl Into<String>,
        transport: Arc<dyn GitTransfer>,
    ) -> Result<Self> {
        let mut repo = Self::open(root, node_id)?;
        repo.set_git_transfer(transport)?;
        Ok(repo)
    }

    pub fn set_git_transfer(&mut self, transport: Arc<dyn GitTransfer>) -> Result<()> {
        if self.primary {
            return Err(FleetError::Invalid(
                "remote Git transport is only available for node repositories".into(),
            ));
        }
        self.git_transfer = Some(transport);
        Ok(())
    }

    fn fetch_origin_at(&self, repo: &Path) -> Result<()> {
        match &self.git_transfer {
            Some(transport) => transport.fetch(repo).map_err(FleetError::Transfer),
            None => {
                git(repo, ["fetch", "origin"])?;
                Ok(())
            }
        }
    }

    fn push_origin_at(&self, repo: &Path, branch: &str) -> Result<()> {
        match &self.git_transfer {
            Some(transport) => transport.push(repo, branch).map_err(FleetError::Transfer),
            None => {
                git(
                    repo,
                    ["push", "origin", &format!("HEAD:refs/heads/{branch}")],
                )?;
                Ok(())
            }
        }
    }

    pub fn open_primary(root: impl AsRef<Path>, node_id: impl Into<String>) -> Result<Self> {
        let repo = Self::open(root, node_id)?;
        if !repo.primary {
            return Err(FleetError::Invalid(
                "fleet repository is not a primary".into(),
            ));
        }
        Ok(repo)
    }

    pub fn open_node(
        root: impl AsRef<Path>,
        node_id: impl Into<String>,
        expected_origin: &str,
    ) -> Result<Self> {
        let repo = Self::open(root, node_id)?;
        if repo.primary {
            return Err(FleetError::Invalid("fleet repository is not a node".into()));
        }
        if repo.origin != expected_origin {
            return Err(FleetError::Invalid("fleet node origin mismatch".into()));
        }
        Ok(repo)
    }

    pub fn set_domain_validator<F>(&mut self, validator: F)
    where
        F: Fn(&[Record]) -> std::result::Result<(), String> + Send + Sync + 'static,
    {
        self.domain_validator = Some(Arc::new(validator));
    }

    fn validate_candidate(&self, records: &[Record]) -> Result<()> {
        let validator = self.domain_validator.as_ref().ok_or_else(|| {
            FleetError::Invalid("primary updates require a domain validator".into())
        })?;
        validator(records)
            .map_err(|error| FleetError::Invalid(format!("domain validation failed: {error}")))
    }

    pub fn repo_path(&self) -> &Path {
        &self.repo
    }
    pub fn data_root(&self) -> &Path {
        &self.data_root
    }
    pub fn origin(&self) -> &str {
        &self.origin
    }
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn head(&self) -> Result<String> {
        let output = git(&self.repo, ["rev-parse", "HEAD"])?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    pub fn list_records(&self) -> Result<Vec<Record>> {
        records_at_tree(&self.repo, "HEAD")
    }

    pub fn commit_record(
        &mut self,
        record: Record,
        expected_revision: Option<&str>,
    ) -> Result<Record> {
        self.commit_records(vec![(record, expected_revision.map(str::to_owned))])?
            .into_iter()
            .next()
            .ok_or_else(|| FleetError::Invalid("single-record commit returned no record".into()))
    }

    /// Validate and publish a group of records as one atomic Git commit.
    /// Each expected revision is checked against the same pre-commit HEAD.
    pub fn commit_records(
        &mut self,
        changes: Vec<(Record, Option<String>)>,
    ) -> Result<Vec<Record>> {
        self.commit_records_with_blobs(changes, Vec::new())
    }

    /// Atomically commit record metadata and any raw content-addressed Git
    /// blobs needed by those records. Blob bytes never enter record JSON.
    pub fn commit_records_with_blobs(
        &mut self,
        changes: Vec<(Record, Option<String>)>,
        blob_files: Vec<BlobFile>,
    ) -> Result<Vec<Record>> {
        if !unmerged_paths(&self.repo)?.is_empty()
            || (self.primary && !self.conflicts()?.is_empty())
        {
            return Err(FleetError::Conflict(
                "resolve pending Git conflicts before committing".into(),
            ));
        }
        if !staged_paths(&self.repo)?.is_empty() {
            return Err(FleetError::Conflict(
                "recover pending staged Git changes before committing".into(),
            ));
        }
        if changes.is_empty() {
            if !blob_files.is_empty() {
                return Err(FleetError::Invalid(
                    "blob sources require matching record metadata in the same batch".into(),
                ));
            }
            return Ok(Vec::new());
        }
        if self.primary && self.domain_validator.is_none() {
            return Err(FleetError::Invalid(
                "primary writes require a domain validator".into(),
            ));
        }
        let current_records = self.list_records()?;
        let mut blob_sources = BTreeMap::new();
        for source in blob_files {
            if !is_sha256(&source.sha256) || source.size_bytes > MAX_BLOB_SIZE {
                return Err(FleetError::Invalid("invalid blob file metadata".into()));
            }
            if blob_sources.insert(source.sha256.clone(), source).is_some() {
                return Err(FleetError::Invalid("duplicate blob file in batch".into()));
            }
        }
        let mut identities = BTreeSet::new();
        let mut prepared = Vec::with_capacity(changes.len());
        for (mut record, expected_revision) in changes {
            validate_record(&record)?;
            if !identities.insert(record.entity_id()) {
                return Err(FleetError::Invalid(
                    "a batch cannot contain the same origin-qualified record more than once".into(),
                ));
            }
            let current_record = current_records
                .iter()
                .find(|saved| saved.entity_id() == record.entity_id());
            if let Some(expected) = expected_revision.as_deref() {
                if current_record.is_none()
                    && current_records
                        .iter()
                        .any(|saved| saved.id == record.id && saved.revision == expected)
                {
                    return Err(FleetError::Invalid(
                        "record origin identity cannot change during an edit".into(),
                    ));
                }
            }
            if current_record.is_some_and(|saved| saved.kind != record.kind) {
                return Err(FleetError::Invalid(
                    "record kind cannot change for an existing id".into(),
                ));
            }
            let current = current_record.map(|saved| saved.revision.clone());
            if current.as_deref() != expected_revision.as_deref() {
                return Err(FleetError::RevisionConflict {
                    id: record.id,
                    expected: expected_revision,
                    actual: current,
                });
            }
            record.base_revision = expected_revision;
            record.revision = record_digest(&record)?;
            prepared.push(record);
        }

        for record in prepared
            .iter()
            .filter(|record| record.kind == RecordKind::Blob)
        {
            let metadata = parse_blob_record(record)?;
            let exists =
                raw_blob_matches(&self.repo, "HEAD", &metadata.sha256, metadata.size_bytes)?;
            if let Some(source) = blob_sources.get(&metadata.sha256) {
                if source.size_bytes != metadata.size_bytes {
                    return Err(FleetError::Invalid(
                        "blob source size differs from its record".into(),
                    ));
                }
            } else if !exists {
                return Err(FleetError::Invalid(format!(
                    "new blob record {} requires its source file in the same batch",
                    metadata.sha256
                )));
            }
        }
        for sha in blob_sources.keys() {
            if !prepared
                .iter()
                .any(|record| record.kind == RecordKind::Blob && record.id == *sha)
                && !current_records.iter().any(|record| {
                    record.kind == RecordKind::Blob && record.id == *sha && !record.deleted
                })
            {
                return Err(FleetError::Invalid(format!(
                    "blob source {sha} has no matching metadata record"
                )));
            }
        }

        let mut snapshots = Vec::with_capacity(prepared.len());
        let mut paths = Vec::with_capacity(prepared.len());
        for record in &prepared {
            let path = record_path(record);
            snapshots.push(snapshot_record_file(&self.repo, &path)?);
            paths.push(path);
        }

        let mut added_blob_paths = Vec::new();
        let blob_paths = blob_sources
            .keys()
            .map(|sha| blob_path(sha))
            .collect::<Vec<_>>();

        let staged = (|| -> Result<()> {
            for (sha, source) in &blob_sources {
                let path = blob_path(sha);
                if raw_blob_matches(&self.repo, "HEAD", sha, source.size_bytes)? {
                    continue;
                }
                copy_verified_blob(&self.repo, source, &path)?;
                added_blob_paths.push(path.clone());
                git(&self.repo, ["add", "--", path.as_str()])?;
            }
            for (record, path) in prepared.iter().zip(&paths) {
                write_record_file(&self.repo, path, &serde_json::to_vec_pretty(record)?)?;
                git(&self.repo, ["add", "--", path.as_str()])?;
            }
            validate_repository_records(&self.repo)?;
            if self.primary {
                let candidate = records_at_index(&self.repo)?;
                self.validate_candidate(&candidate)?;
            }
            let summary = prepared
                .iter()
                .map(|record| format!("{} {}", record.kind.path_segment(), record.id))
                .collect::<Vec<_>>()
                .join(", ");
            git(
                &self.repo,
                ["commit", "-m", &format!("Update fleet records: {summary}")],
            )?;
            Ok(())
        })();
        if let Err(error) = staged {
            let rollback = restore_record_files(&self.repo, &paths, &snapshots);
            for path in &added_blob_paths {
                let target = self.repo.join(path);
                match fs::symlink_metadata(&target) {
                    Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                        let _ = fs::remove_file(target);
                    }
                    _ => {}
                }
            }
            if !blob_paths.is_empty() {
                let _ = git(
                    &self.repo,
                    std::iter::once("reset")
                        .chain(std::iter::once("--"))
                        .chain(blob_paths.iter().map(String::as_str)),
                );
            }
            if let Err(rollback_error) = rollback {
                return Err(FleetError::Invalid(format!(
                    "{error}; restoring the pre-commit working tree failed: {rollback_error}"
                )));
            }
            return Err(error);
        }
        Ok(prepared)
    }

    pub fn sync(&mut self) -> Result<SyncOutcome> {
        if self.primary && !self.conflicts()?.is_empty() {
            return Ok(SyncOutcome::Conflicted {
                records: self.conflicts()?.into_iter().map(|c| c.id).collect(),
            });
        }
        self.fetch_origin_at(&self.repo)?;
        if let Some(outcome) = self.finish_pending_resolution()? {
            return Ok(outcome);
        }
        if self.primary {
            self.sync_primary()
        } else {
            self.sync_node()
        }
    }

    pub fn conflicts(&self) -> Result<Vec<Conflict>> {
        let repo = self.conflict_repo();
        let paths = unmerged_paths(&repo)?;
        let mut conflicts = Vec::new();
        for path in paths {
            if !path.starts_with("records/") || !path.ends_with(".json") {
                continue;
            }
            let id = read_index_record(&repo, 2, &path)?
                .or(read_index_record(&repo, 3, &path)?)
                .or(read_index_record(&repo, 1, &path)?)
                .map(|r| r.id.clone());
            if let Some(id) = id {
                let Some(record) = read_index_record(&repo, 2, &path)?
                    .or(read_index_record(&repo, 3, &path)?)
                    .or(read_index_record(&repo, 1, &path)?)
                else {
                    continue;
                };
                conflicts.push(Conflict {
                    entity_id: record.entity_id(),
                    id,
                    base: read_index_record(&repo, 1, &path)?,
                    current: read_index_record(&repo, 2, &path)?,
                    incoming: read_index_record(&repo, 3, &path)?,
                });
            }
        }
        Ok(conflicts)
    }

    pub fn resolve(&mut self, id: &str, record: Record) -> Result<Record> {
        if record.id != id {
            return Err(FleetError::Invalid(
                "resolution record id must match conflict id".into(),
            ));
        }
        let mut resolved = self.resolve_batch(vec![record], Vec::new())?;
        Ok(resolved.remove(0))
    }

    /// Resolve indexed conflicts and add new, previously absent proof records
    /// as one staged operation. All records are validated before writing, and
    /// the original index, affected files, and pending marker are restored if
    /// staging or final-candidate validation fails.
    pub fn resolve_batch(
        &mut self,
        mut resolutions: Vec<Record>,
        mut additions: Vec<Record>,
    ) -> Result<Vec<Record>> {
        if resolutions.is_empty() && additions.is_empty() {
            return Err(FleetError::Invalid(
                "resolution batch must not be empty".into(),
            ));
        }
        let conflict_repo = self.conflict_repo();
        let conflicts = self.conflicts()?;
        let conflict_paths = unmerged_paths(&conflict_repo)?;
        let mut paths = BTreeSet::new();
        let mut identities = BTreeSet::new();
        for record in &mut resolutions {
            validate_record(record)?;
            record.revision = record_digest(record)?;
            let path = record_path(record);
            if !conflict_paths
                .iter()
                .any(|conflict_path| conflict_path == &path)
            {
                return Err(FleetError::Conflict(format!(
                    "no Git conflict for record {}",
                    record.id
                )));
            }
            if !conflicts.iter().any(|conflict| {
                conflict.entity_id == record.entity_id() && conflict.id == record.id
            }) {
                return Err(FleetError::Invalid(
                    "resolution must preserve the conflicted record origin identity".into(),
                ));
            }
            if !paths.insert(path) || !identities.insert(record.entity_id()) {
                return Err(FleetError::Invalid(
                    "duplicate record in resolution batch".into(),
                ));
            }
        }
        for record in &mut additions {
            validate_record(record)?;
            record.revision = record_digest(record)?;
            let path = record_path(record);
            if !paths.insert(path.clone()) || !identities.insert(record.entity_id()) {
                return Err(FleetError::Invalid(
                    "duplicate record in resolution batch".into(),
                ));
            }
            let tracked = git(&conflict_repo, ["ls-files", "--stage", "--", path.as_str()])?;
            if !tracked.stdout.is_empty() {
                return Err(FleetError::Conflict(format!(
                    "proof record {} already exists in the conflict index",
                    record.id
                )));
            }
            if records_at_tree(&conflict_repo, "HEAD")?
                .iter()
                .any(|existing| existing.entity_id() == record.entity_id())
            {
                return Err(FleetError::Conflict(format!(
                    "proof record {} already exists",
                    record.id
                )));
            }
            if snapshot_record_file(&conflict_repo, &path)?.bytes.is_some() {
                return Err(FleetError::Conflict(format!(
                    "proof record {} would overwrite an existing worktree file",
                    record.id
                )));
            }
        }

        let paths = paths.into_iter().collect::<Vec<_>>();
        let snapshots = paths
            .iter()
            .map(|path| snapshot_record_file(&conflict_repo, path))
            .collect::<Result<Vec<_>>>()?;
        let index_path = git_index_path(&conflict_repo)?;
        let index_before = fs::read(&index_path)?;
        let marker = self.meta_root.join("resolution.pending");
        let marker_before = match fs::read(&marker) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };

        let stage_result = (|| -> Result<()> {
            fs::write(&marker, if self.primary { "primary" } else { "node" })?;
            for record in resolutions.iter().chain(&additions) {
                let path = record_path(record);
                write_record_file(&conflict_repo, &path, &serde_json::to_vec_pretty(record)?)?;
                git(&conflict_repo, ["add", "--", path.as_str()])?;
            }
            if unmerged_paths(&conflict_repo)?.is_empty() {
                validate_repository_records(&conflict_repo)?;
                if self.primary {
                    self.validate_candidate(&records_at_index(&conflict_repo)?)?;
                }
            }
            Ok(())
        })();
        if let Err(error) = stage_result {
            let restore_files = restore_record_files(&conflict_repo, &paths, &snapshots);
            let restore_index = fs::write(&index_path, &index_before).map_err(FleetError::from);
            let restore_marker = restore_optional_file(&marker, marker_before.as_deref());
            for restore in [restore_files, restore_index, restore_marker] {
                if let Err(rollback_error) = restore {
                    return Err(FleetError::Invalid(format!(
                        "{error}; restoring conflict state failed: {rollback_error}"
                    )));
                }
            }
            return Err(error);
        }

        let mut resolved = resolutions;
        resolved.extend(additions);
        if unmerged_paths(&conflict_repo)?.is_empty() {
            self.finish_pending_resolution()?;
        }
        Ok(resolved)
    }

    fn finish_pending_resolution(&mut self) -> Result<Option<SyncOutcome>> {
        let marker = self.meta_root.join("resolution.pending");
        if !marker.exists() {
            return Ok(None);
        }
        let marker_role = fs::read_to_string(&marker)?;
        let expected_role = if self.primary { "primary" } else { "node" };
        if marker_role != expected_role {
            return Err(FleetError::Invalid(
                "pending resolution role does not match this fleet checkout".into(),
            ));
        }
        let conflict_repo = self.meta_root.join("merge-conflict");
        if !conflict_repo.exists() {
            fs::remove_file(marker)?;
            return Ok(None);
        }
        if !unmerged_paths(&conflict_repo)?.is_empty() {
            return Ok(None);
        }
        validate_repository_records(&conflict_repo)?;
        if self.primary {
            self.validate_candidate(&records_at_index(&conflict_repo)?)?;
        }
        if git(&conflict_repo, ["rev-parse", "MERGE_HEAD"]).is_ok() {
            git(
                &conflict_repo,
                ["commit", "-m", "Complete fleet conflict resolution"],
            )?;
        }
        if self.primary {
            git(
                &self.repo,
                ["fetch", conflict_repo.to_str().unwrap(), "HEAD"],
            )?;
            git(&self.repo, ["merge", "--ff-only", "FETCH_HEAD"])?;
            self.push_origin_at(&self.repo, "main")?;
            let commit = self.head()?;
            let records = self.list_records()?.len();
            fs::remove_file(marker)?;
            let _ = fs::remove_dir_all(&conflict_repo);
            let _ = fs::remove_file(self.meta_root.join("conflict-branch"));
            return Ok(Some(SyncOutcome::Merged { commit, records }));
        }

        // Reconcile any node edits committed while the conflict was being
        // reviewed before advancing the live branch.
        git(
            &conflict_repo,
            ["fetch", self.repo.to_str().unwrap(), "HEAD"],
        )?;
        if git(&conflict_repo, ["merge", "--no-edit", "FETCH_HEAD"]).is_err() {
            return Ok(None);
        }
        validate_repository_records(&conflict_repo)?;
        if git(&conflict_repo, ["rev-parse", "MERGE_HEAD"]).is_ok() {
            git(
                &conflict_repo,
                [
                    "commit",
                    "-m",
                    "Merge node edits made during conflict review",
                ],
            )?;
        }
        git(
            &self.repo,
            ["fetch", conflict_repo.to_str().unwrap(), "HEAD"],
        )?;
        git(&self.repo, ["merge", "--ff-only", "FETCH_HEAD"])?;
        let branch = format!("nodes/{}", self.node_id);
        self.push_origin_at(&self.repo, &branch)?;
        fs::remove_file(marker)?;
        let _ = fs::remove_dir_all(&conflict_repo);
        Ok(Some(SyncOutcome::Published { branch }))
    }

    fn sync_node(&mut self) -> Result<SyncOutcome> {
        let branch = format!("nodes/{}", self.node_id);
        if self.meta_root.join("resolution.pending").exists()
            && !unmerged_paths(&self.conflict_repo())?.is_empty()
        {
            return Ok(SyncOutcome::Conflicted {
                records: self
                    .conflicts()?
                    .into_iter()
                    .map(|conflict| conflict.id)
                    .collect(),
            });
        }
        // Publish the isolated branch first so a merge conflict still leaves both
        // node variants available to the primary for review.
        self.push_origin_at(&self.repo, &branch)?;
        let staging = self.meta_root.join("merge-staging");
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        git(
            &self.meta_root,
            [
                "clone",
                self.repo.to_str().unwrap(),
                staging.to_str().unwrap(),
            ],
        )?;
        configure_repo(&staging, &self.node_id, &self.meta_root)?;
        git(&staging, ["remote", "set-url", "origin", &self.origin])?;
        self.fetch_origin_at(&staging)?;
        let local_head = String::from_utf8_lossy(&git(&staging, ["rev-parse", "HEAD"])?.stdout)
            .trim()
            .to_owned();
        let merge = if git(
            &staging,
            ["merge-base", "--is-ancestor", &local_head, "origin/main"],
        )
        .is_ok()
        {
            git(&staging, ["merge", "--ff-only", "origin/main"])
        } else {
            git(&staging, ["merge", "--no-commit", "--no-ff", "origin/main"])
        };
        if merge.is_err() && !auto_merge_conflicts(&staging)? {
            let found = unmerged_record_ids(&staging)?;
            let retained = self.meta_root.join("merge-conflict");
            if retained.exists() {
                fs::remove_dir_all(&retained)?;
            }
            fs::rename(&staging, &retained)?;
            return Ok(SyncOutcome::Conflicted { records: found });
        }
        validate_repository_records(&staging)?;
        if git(&staging, ["rev-parse", "MERGE_HEAD"]).is_ok() {
            git(
                &staging,
                ["commit", "-m", "Merge origin/main into node branch"],
            )?;
        }
        git(&self.repo, ["fetch", staging.to_str().unwrap(), "HEAD"])?;
        git(&self.repo, ["merge", "--ff-only", "FETCH_HEAD"])?;
        validate_repository_records(&self.repo)?;
        self.push_origin_at(&self.repo, &branch)?;
        let _ = fs::remove_dir_all(self.meta_root.join("merge-conflict"));
        let _ = fs::remove_file(self.meta_root.join("conflict-branch"));
        let _ = fs::remove_dir_all(staging);
        Ok(SyncOutcome::Published { branch })
    }

    fn sync_primary(&mut self) -> Result<SyncOutcome> {
        validate_repository_records(&self.repo)?;
        self.validate_candidate(&self.list_records()?)?;
        let stale_conflict = self.meta_root.join("merge-conflict");
        if stale_conflict.exists() && unmerged_paths(&stale_conflict)?.is_empty() {
            fs::remove_dir_all(stale_conflict)?;
        }
        // Local primary edits belong on authoritative main before importing
        // branches; the staged incoming merges below remain validation-gated.
        self.push_origin_at(&self.repo, "main")?;
        let refs = git(
            &self.repo,
            [
                "for-each-ref",
                "--format=%(refname:short)",
                "refs/remotes/origin/nodes",
            ],
        )?;
        let branches = String::from_utf8_lossy(&refs.stdout)
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let mut merged = 0;
        for branch in branches {
            if git(&self.repo, ["merge-base", "--is-ancestor", &branch, "main"]).is_ok() {
                continue;
            }
            let incoming =
                String::from_utf8_lossy(&git(&self.repo, ["rev-parse", &branch])?.stdout)
                    .trim()
                    .to_owned();
            let source_node = branch.strip_prefix("origin/nodes/").ok_or_else(|| {
                FleetError::Invalid(format!("unexpected fleet node branch name: {branch}"))
            })?;
            let staging = self.meta_root.join("merge-staging");
            if staging.exists() {
                fs::remove_dir_all(&staging)?;
            }
            git(
                &self.meta_root,
                [
                    "clone",
                    self.repo.to_str().unwrap(),
                    staging.to_str().unwrap(),
                ],
            )?;
            configure_repo(&staging, &self.node_id, &self.meta_root)?;
            git(&staging, ["checkout", "main"])?;
            let base =
                String::from_utf8_lossy(&git(&staging, ["merge-base", "main", &incoming])?.stdout)
                    .trim()
                    .to_owned();
            validate_projection_ownership(&staging, &base, &incoming, source_node)?;
            let merge = git(&staging, ["merge", "--no-commit", "--no-ff", &incoming]);
            if merge.is_err() {
                let original_paths = unmerged_paths(&staging)?;
                let original_files = original_paths
                    .iter()
                    .map(|path| snapshot_record_file(&staging, path))
                    .collect::<Result<Vec<_>>>()?;
                let original_index_path = git_index_path(&staging)?;
                let original_index = fs::read(&original_index_path)?;
                if !auto_merge_conflicts(&staging)? {
                    // Retain the in-progress merge so base/current/incoming variants remain addressable.
                    fs::write(self.meta_root.join("conflict-branch"), &branch)?;
                    let found = unmerged_record_ids(&staging)?;
                    fs::rename(&staging, self.meta_root.join("merge-conflict"))?;
                    // The primary checkout remains on its node branch and its main ref is untouched.
                    return Ok(SyncOutcome::Conflicted { records: found });
                }
                let validation = validate_repository_records(&staging)
                    .and_then(|_| self.validate_candidate(&records_at_index(&staging)?));
                if let Err(error) = validation {
                    restore_record_files(&staging, &original_paths, &original_files)?;
                    fs::write(&original_index_path, &original_index)?;
                    fs::write(self.meta_root.join("conflict-branch"), &branch)?;
                    let found = unmerged_record_ids(&staging)?;
                    fs::rename(&staging, self.meta_root.join("merge-conflict"))?;
                    // A disjoint automatic composition still needs explicit review when the
                    // composed record fails the domain policy.
                    let _ = error;
                    return Ok(SyncOutcome::Conflicted { records: found });
                }
            }
            validate_repository_records(&staging)?;
            self.validate_candidate(&records_at_index(&staging)?)?;
            git(
                &staging,
                ["commit", "-m", &format!("Merge {branch} into main")],
            )?;
            let candidate = String::from_utf8_lossy(&git(&staging, ["rev-parse", "HEAD"])?.stdout)
                .trim()
                .to_owned();
            git(&self.repo, ["fetch", staging.to_str().unwrap(), "HEAD"])?;
            git(&self.repo, ["checkout", "main"])?;
            git(&self.repo, ["merge", "--ff-only", "FETCH_HEAD"])?;
            let _ = candidate;
            self.push_origin_at(&self.repo, "main")?;
            merged += 1;
            let _ = fs::remove_dir_all(&staging);
        }
        if merged == 0 {
            Ok(SyncOutcome::UpToDate)
        } else {
            Ok(SyncOutcome::Merged {
                commit: self.head()?,
                records: self.list_records()?.len(),
            })
        }
    }

    fn conflict_repo(&self) -> PathBuf {
        let staging = self.meta_root.join("merge-conflict");
        if staging.exists() {
            staging
        } else {
            self.repo.clone()
        }
    }
}

struct RecordFileSnapshot {
    bytes: Option<Vec<u8>>,
    permissions: Option<fs::Permissions>,
}

static NEXT_RECORD_TEMP: AtomicU64 = AtomicU64::new(0);

fn record_target(repo: &Path, path: &str, create_parents: bool) -> Result<PathBuf> {
    if !path.starts_with("records/") {
        return Err(FleetError::Invalid(
            "record path is outside records/".into(),
        ));
    }
    let relative = Path::new(path);
    let components = relative.components().collect::<Vec<_>>();
    if components.len() != 3
        || components
            .iter()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(FleetError::Invalid("unsafe record path".into()));
    }
    let mut parent = repo.to_path_buf();
    for component in &components[..components.len() - 1] {
        let Component::Normal(segment) = component else {
            unreachable!()
        };
        parent.push(segment);
        match fs::symlink_metadata(&parent) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(FleetError::Invalid(format!(
                    "record path contains a symlink: {}",
                    parent.display()
                )));
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(FleetError::Invalid(format!(
                    "record path parent is not a directory: {}",
                    parent.display()
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound && create_parents => {
                fs::create_dir(&parent)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    let target = repo.join(relative);
    match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(FleetError::Invalid(format!(
            "record path is a symlink: {}",
            target.display()
        ))),
        Ok(metadata) if metadata.is_file() => Ok(target),
        Ok(_) => Err(FleetError::Invalid(format!(
            "record path is not a regular file: {}",
            target.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(target),
        Err(error) => Err(error.into()),
    }
}

fn snapshot_record_file(repo: &Path, path: &str) -> Result<RecordFileSnapshot> {
    let target = record_target(repo, path, false)?;
    match fs::symlink_metadata(&target) {
        Ok(metadata) => Ok(RecordFileSnapshot {
            bytes: Some(fs::read(&target)?),
            permissions: Some(metadata.permissions()),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(RecordFileSnapshot {
            bytes: None,
            permissions: None,
        }),
        Err(error) => Err(error.into()),
    }
}

fn write_record_file(repo: &Path, path: &str, contents: &[u8]) -> Result<()> {
    let target = record_target(repo, path, true)?;
    let parent = target
        .parent()
        .ok_or_else(|| FleetError::Invalid("record path has no parent directory".into()))?;
    let name = target
        .file_name()
        .ok_or_else(|| FleetError::Invalid("record path has no file name".into()))?
        .to_string_lossy();
    let mut temp = None;
    for _ in 0..8 {
        let suffix = NEXT_RECORD_TEMP.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{name}.brr-pending-{}-{suffix}",
            std::process::id()
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                let write_result = (|| -> io::Result<()> {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        file.set_permissions(fs::Permissions::from_mode(0o644))?;
                    }
                    file.write_all(contents)?;
                    file.sync_all()
                })();
                if let Err(error) = write_result {
                    let _ = fs::remove_file(&candidate);
                    return Err(error.into());
                }
                temp = Some(candidate);
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    let temp =
        temp.ok_or_else(|| FleetError::Invalid("could not allocate a record staging file".into()))?;
    if let Err(error) = fs::rename(&temp, &target) {
        let _ = fs::remove_file(&temp);
        return Err(error.into());
    }
    Ok(())
}

fn restore_record_files(
    repo: &Path,
    paths: &[String],
    snapshots: &[RecordFileSnapshot],
) -> Result<()> {
    let reset = git(
        repo,
        std::iter::once("reset")
            .chain(std::iter::once("--"))
            .chain(paths.iter().map(String::as_str)),
    );
    for (path, snapshot) in paths.iter().zip(snapshots) {
        let target = record_target(repo, path, snapshot.bytes.is_some())?;
        match &snapshot.bytes {
            Some(bytes) => {
                write_record_file(repo, path, bytes)?;
                if let Some(permissions) = &snapshot.permissions {
                    fs::set_permissions(&target, permissions.clone())?;
                }
            }
            None => match fs::symlink_metadata(&target) {
                Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                    fs::remove_file(target)?;
                }
                Ok(_) => {
                    return Err(FleetError::Invalid(format!(
                        "refusing to remove a non-regular record path: {}",
                        target.display()
                    )));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            },
        }
    }
    reset?;
    Ok(())
}

fn validate_node_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id == "."
        || id == ".."
        || id.starts_with('.')
        || id.ends_with('.')
        || id.ends_with(".lock")
        || id.contains("..")
        || id.contains("@{")
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
    {
        return Err(FleetError::Invalid(
            "node id must contain only ASCII letters, digits, dot, underscore, or hyphen".into(),
        ));
    }
    Ok(())
}

fn validate_record(record: &Record) -> Result<()> {
    if record.schema_version != SCHEMA_VERSION {
        return Err(FleetError::Invalid(format!(
            "unsupported record schema {}",
            record.schema_version
        )));
    }
    if record.id.trim().is_empty() || record.id.len() > 512 {
        return Err(FleetError::Invalid(
            "record id must be 1 to 512 bytes".into(),
        ));
    }
    validate_node_id(&record.origin_node_id)?;
    if record.kind == RecordKind::Blob {
        validate_blob_record(record)?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BlobRecordContent {
    sha256: String,
    size_bytes: u64,
}

const MAX_BLOB_SIZE: u64 = 50 * 1024 * 1024;

fn validate_blob_record(record: &Record) -> Result<()> {
    let blob = parse_blob_record(record)?;
    if record.deleted || !is_sha256(&blob.sha256) || record.id != blob.sha256 {
        return Err(FleetError::Invalid(
            "blob records cannot be deleted and their ID must equal lowercase SHA-256".into(),
        ));
    }
    if blob.size_bytes > MAX_BLOB_SIZE {
        return Err(FleetError::Invalid(
            "blob exceeds the 50 MiB attachment limit".into(),
        ));
    }
    Ok(())
}

fn parse_blob_record(record: &Record) -> Result<BlobRecordContent> {
    Ok(serde_json::from_value(record.content.clone())?)
}

fn blob_path(sha256: &str) -> String {
    format!("blobs/{sha256}")
}

fn raw_blob_matches(repo: &Path, treeish: &str, sha256: &str, size_bytes: u64) -> Result<bool> {
    let path = blob_path(sha256);
    let tree = git(repo, ["ls-tree", "-z", treeish, "--", path.as_str()])?;
    let Some(entry) = tree
        .stdout
        .split(|b| *b == 0)
        .find(|entry| !entry.is_empty())
    else {
        return Ok(false);
    };
    let separator = entry
        .iter()
        .position(|b| *b == b'\t')
        .ok_or_else(|| FleetError::Invalid("malformed Git blob tree entry".into()))?;
    let metadata = String::from_utf8_lossy(&entry[..separator]);
    let mut fields = metadata.split_whitespace();
    if fields.next() != Some("100644") || fields.next() != Some("blob") {
        return Err(FleetError::Invalid(
            "content-addressed blobs must be regular non-executable files".into(),
        ));
    }
    let object = fields
        .next()
        .ok_or_else(|| FleetError::Invalid("Git blob object is missing".into()))?;
    let size_output = git(repo, ["cat-file", "-s", object])?;
    let size = String::from_utf8_lossy(&size_output.stdout)
        .trim()
        .parse::<u64>()
        .map_err(|_| FleetError::Invalid("Git blob size is malformed".into()))?;
    if size != size_bytes || size > MAX_BLOB_SIZE {
        return Err(FleetError::Invalid(
            "Git blob size differs from its metadata".into(),
        ));
    }
    let spec = format!("{treeish}:{path}");
    let contents = git(repo, ["show", spec.as_str()])?;
    if hex(&Sha256::digest(&contents.stdout)) != sha256 {
        return Err(FleetError::Invalid(
            "Git blob SHA-256 differs from its content address".into(),
        ));
    }
    Ok(true)
}

fn copy_verified_blob(repo: &Path, source: &BlobFile, path: &str) -> Result<()> {
    if !is_sha256(&source.sha256) || source.size_bytes > MAX_BLOB_SIZE {
        return Err(FleetError::Invalid("invalid blob source metadata".into()));
    }
    let target = blob_target(repo, path, true)?;
    let parent = target
        .parent()
        .ok_or_else(|| FleetError::Invalid("blob target has no parent".into()))?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let canonical_source = fs::canonicalize(&source.source_path)?;
    if canonical_source != source.source_path {
        return Err(FleetError::Invalid(
            "blob source path must be canonical and contain no symbolic links".into(),
        ));
    }
    let mut input = options.open(&source.source_path)?;
    let metadata = input.metadata()?;
    if !metadata.is_file() || metadata.len() != source.size_bytes {
        return Err(FleetError::Invalid(
            "blob source is not a regular file of the declared size".into(),
        ));
    }
    let name = target
        .file_name()
        .ok_or_else(|| FleetError::Invalid("blob target has no filename".into()))?
        .to_string_lossy();
    let mut temp = None;
    for _ in 0..8 {
        let suffix = NEXT_RECORD_TEMP.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{name}.brr-pending-{}-{suffix}",
            std::process::id()
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut output) => {
                let copy_result = (|| -> Result<()> {
                    let mut hasher = Sha256::new();
                    let mut copied = 0u64;
                    let mut buffer = [0u8; 64 * 1024];
                    loop {
                        let read = io::Read::read(&mut input, &mut buffer)?;
                        if read == 0 {
                            break;
                        }
                        copied = copied.checked_add(read as u64).ok_or_else(|| {
                            FleetError::Invalid("blob source size overflow".into())
                        })?;
                        if copied > source.size_bytes || copied > MAX_BLOB_SIZE {
                            return Err(FleetError::Invalid(
                                "blob source grew beyond its declared size".into(),
                            ));
                        }
                        hasher.update(&buffer[..read]);
                        output.write_all(&buffer[..read])?;
                    }
                    if copied != source.size_bytes || hex(&hasher.finalize()) != source.sha256 {
                        return Err(FleetError::Invalid(
                            "blob source size or SHA-256 mismatch".into(),
                        ));
                    }
                    output.sync_all()?;
                    Ok(())
                })();
                if let Err(error) = copy_result {
                    drop(output);
                    let _ = fs::remove_file(&candidate);
                    return Err(error);
                }
                drop(output);
                temp = Some(candidate);
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    let temp =
        temp.ok_or_else(|| FleetError::Invalid("could not allocate a blob staging file".into()))?;
    match fs::symlink_metadata(&target) {
        Ok(_) => {
            let _ = fs::remove_file(&temp);
            return Err(FleetError::Invalid(
                "refusing to replace an existing blob path".into(),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            let _ = fs::remove_file(&temp);
            return Err(error.into());
        }
    }
    if let Err(error) = fs::hard_link(&temp, &target) {
        let _ = fs::remove_file(&temp);
        return Err(error.into());
    }
    if let Err(error) = fs::remove_file(&temp) {
        let _ = fs::remove_file(&target);
        return Err(error.into());
    }
    Ok(())
}

fn blob_target(repo: &Path, path: &str, create_parent: bool) -> Result<PathBuf> {
    let Some(sha) = path.strip_prefix("blobs/") else {
        return Err(FleetError::Invalid("unapproved blob path".into()));
    };
    if !is_sha256(sha) || path != blob_path(sha) {
        return Err(FleetError::Invalid("unapproved blob path".into()));
    }
    let dir = repo.join("blobs");
    match fs::symlink_metadata(&dir) {
        Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
            return Err(FleetError::Invalid(
                "blob directory is not a real directory".into(),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound && create_parent => {
            fs::create_dir(&dir)?
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(dir.join(sha))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn record_path(record: &Record) -> String {
    format!(
        "records/{}/{}.json",
        record.kind.path_segment(),
        id_digest(&format!("{}\0{}", record.origin_node_id, record.id))
    )
}

fn id_digest(id: &str) -> String {
    hex(&Sha256::digest(id.as_bytes()))
}

fn record_digest(record: &Record) -> Result<String> {
    let mut copy = record.clone();
    copy.revision.clear();
    Ok(hex(&Sha256::digest(serde_json::to_vec(&copy)?)))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn records_at_tree(repo: &Path, treeish: &str) -> Result<Vec<Record>> {
    let tree = resolve_tree_oid(repo, treeish)?;
    let repo_key = fs::canonicalize(repo)?;
    let key = ValidatedTreeKey {
        repo: repo_key,
        tree: tree.clone(),
    };
    if let Some(records) = validated_tree_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
    {
        return Ok(records);
    }

    let records = records_at_tree_uncached(repo, &tree)?;
    let metadata_bytes = estimated_metadata_bytes(&records);
    if metadata_bytes <= MAX_VALIDATED_TREE_ENTRY_BYTES {
        validated_tree_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, records.clone(), metadata_bytes);
    }
    Ok(records)
}

fn records_at_tree_uncached(repo: &Path, tree_oid: &str) -> Result<Vec<Record>> {
    let output = git(repo, ["ls-tree", "-r", "-z", tree_oid, "--", "records"])?;
    let mut records = Vec::new();
    for entry in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let separator = entry
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| FleetError::Invalid("malformed Git tree entry".into()))?;
        let (metadata, path) = entry.split_at(separator);
        let path = &path[1..];
        let metadata = String::from_utf8_lossy(metadata);
        let mut parts = metadata.split_whitespace();
        let mode = parts.next().unwrap_or_default();
        let object_type = parts.next().unwrap_or_default();
        let _object = parts.next().unwrap_or_default();
        if mode != "100644" || object_type != "blob" {
            return Err(FleetError::Invalid(
                "fleet records must be regular non-executable files".into(),
            ));
        }
        let path = std::str::from_utf8(path)
            .map_err(|_| FleetError::Invalid("non-UTF8 record path".into()))?;
        if path == "records/.keep" {
            continue;
        }
        if !path.starts_with("records/") || !path.ends_with(".json") {
            return Err(FleetError::Invalid(format!(
                "unapproved record path: {path}"
            )));
        }
        let spec = format!("{tree_oid}:{path}");
        let blob = git(repo, ["show", spec.as_str()])?;
        let record: Record = serde_json::from_slice(&blob.stdout)?;
        validate_record(&record)?;
        if record.revision != record_digest(&record)? {
            return Err(FleetError::Invalid(format!(
                "record revision mismatch at {path}"
            )));
        }
        if record_path(&record) != path {
            return Err(FleetError::Invalid(
                "record path does not match its origin-qualified id".into(),
            ));
        }
        records.push(record);
    }
    records.sort_by_key(Record::entity_id);
    validate_blob_tree(repo, tree_oid, &records)?;
    Ok(records)
}

fn resolve_tree_oid(repo: &Path, treeish: &str) -> Result<String> {
    let spec = format!("{treeish}^{{tree}}");
    let output = git(repo, ["rev-parse", "--verify", "--end-of-options", &spec])?;
    let tree = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !matches!(tree.len(), 40 | 64) || !tree.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(FleetError::Invalid(
            "Git resolved an invalid immutable tree identity".into(),
        ));
    }
    Ok(tree)
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ValidatedTreeKey {
    repo: PathBuf,
    tree: String,
}

#[derive(Default)]
struct ValidatedTreeCache {
    entries: BTreeMap<ValidatedTreeKey, (Vec<Record>, usize)>,
    order: VecDeque<ValidatedTreeKey>,
    metadata_bytes: usize,
}

impl ValidatedTreeCache {
    fn get(&mut self, key: &ValidatedTreeKey) -> Option<Vec<Record>> {
        let (records, _) = self.entries.get(key)?;
        let records = records.clone();
        self.order.retain(|entry| entry != key);
        self.order.push_back(key.clone());
        Some(records)
    }

    fn insert(&mut self, key: ValidatedTreeKey, records: Vec<Record>, bytes: usize) {
        if bytes > MAX_VALIDATED_TREE_ENTRY_BYTES {
            return;
        }
        if let Some((_, old_bytes)) = self.entries.remove(&key) {
            self.metadata_bytes = self.metadata_bytes.saturating_sub(old_bytes);
            self.order.retain(|entry| entry != &key);
        }
        while self.entries.len() >= MAX_VALIDATED_TREE_CACHE_ENTRIES
            || self.metadata_bytes.saturating_add(bytes) > MAX_VALIDATED_TREE_CACHE_BYTES
        {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some((_, old_bytes)) = self.entries.remove(&oldest) {
                self.metadata_bytes = self.metadata_bytes.saturating_sub(old_bytes);
            }
        }
        self.metadata_bytes = self.metadata_bytes.saturating_add(bytes);
        self.order.push_back(key.clone());
        self.entries.insert(key, (records, bytes));
    }

    fn clear_repo(&mut self, repo: &Path) {
        self.entries.retain(|key, (_, bytes)| {
            if key.repo == repo {
                self.metadata_bytes = self.metadata_bytes.saturating_sub(*bytes);
                false
            } else {
                true
            }
        });
        self.order.retain(|key| key.repo != repo);
    }
}

fn validated_tree_cache() -> &'static Mutex<ValidatedTreeCache> {
    static CACHE: OnceLock<Mutex<ValidatedTreeCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(ValidatedTreeCache::default()))
}

fn clear_validated_tree_cache(repo: &Path) -> Result<()> {
    let canonical = fs::canonicalize(repo)?;
    validated_tree_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear_repo(&canonical);
    Ok(())
}

fn estimated_metadata_bytes(records: &[Record]) -> usize {
    struct Counter(usize);
    impl io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    for record in records {
        if serde_json::to_writer(&mut counter, record).is_err() {
            return usize::MAX;
        }
    }
    // Account conservatively for owned JSON/string/vector overhead as well
    // as the serialized record content retained by the cache.
    counter
        .0
        .saturating_mul(2)
        .saturating_add(records.len().saturating_mul(128))
}

fn records_at_index(repo: &Path) -> Result<Vec<Record>> {
    let tree = git(repo, ["write-tree"])?;
    let tree = String::from_utf8_lossy(&tree.stdout).trim().to_owned();
    records_at_tree(repo, &tree)
}

fn validate_repository_records(repo: &Path) -> Result<()> {
    let records = records_at_index(repo)?;
    let mut ids = BTreeSet::new();
    let mut allowed_paths = BTreeSet::from(["records/.keep".to_owned()]);
    for r in records {
        if !ids.insert(r.entity_id()) {
            return Err(FleetError::Invalid(
                "duplicate record id in repository".into(),
            ));
        }
        allowed_paths.insert(record_path(&r));
        if r.kind == RecordKind::Blob {
            let blob = parse_blob_record(&r)?;
            allowed_paths.insert(blob_path(&blob.sha256));
        }
    }
    let tracked = git(repo, ["ls-files", "--cached", "-z"])?;
    for path in String::from_utf8_lossy(&tracked.stdout)
        .split('\0')
        .filter(|p| !p.is_empty())
    {
        if !allowed_paths.contains(path) {
            return Err(FleetError::Invalid(format!(
                "unapproved tracked path in fleet repository: {path}"
            )));
        }
    }
    Ok(())
}

fn validate_blob_tree(repo: &Path, treeish: &str, records: &[Record]) -> Result<()> {
    let mut declared = BTreeMap::<String, u64>::new();
    for record in records
        .iter()
        .filter(|record| record.kind == RecordKind::Blob && !record.deleted)
    {
        let metadata = parse_blob_record(record)?;
        if declared
            .insert(metadata.sha256, metadata.size_bytes)
            .is_some_and(|prior| prior != metadata.size_bytes)
        {
            return Err(FleetError::Invalid(
                "content-addressed blob records disagree on size".into(),
            ));
        }
    }
    let output = git(repo, ["ls-tree", "-r", "-z", treeish, "--", "blobs"])?;
    let mut found = BTreeSet::new();
    for entry in output.stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let tab = entry
            .iter()
            .position(|b| *b == b'\t')
            .ok_or_else(|| FleetError::Invalid("malformed blob Git tree entry".into()))?;
        let metadata = String::from_utf8_lossy(&entry[..tab]);
        let path = std::str::from_utf8(&entry[tab + 1..])
            .map_err(|_| FleetError::Invalid("non-UTF8 blob path".into()))?;
        let Some(sha) = path.strip_prefix("blobs/") else {
            return Err(FleetError::Invalid(format!("unapproved blob path: {path}")));
        };
        if path != blob_path(sha) || !is_sha256(sha) {
            return Err(FleetError::Invalid(format!("unapproved blob path: {path}")));
        }
        let mut fields = metadata.split_whitespace();
        if fields.next() != Some("100644") || fields.next() != Some("blob") {
            return Err(FleetError::Invalid(
                "fleet blobs must be regular non-executable files".into(),
            ));
        }
        let object = fields
            .next()
            .ok_or_else(|| FleetError::Invalid("Git blob object is missing".into()))?;
        let size_out = git(repo, ["cat-file", "-s", object])?;
        let size = String::from_utf8_lossy(&size_out.stdout)
            .trim()
            .parse::<u64>()
            .map_err(|_| FleetError::Invalid("Git blob size is malformed".into()))?;
        if size > MAX_BLOB_SIZE || declared.get(sha) != Some(&size) {
            return Err(FleetError::Invalid(
                "Git blob path has missing or mismatched metadata".into(),
            ));
        }
        let contents = git(repo, ["cat-file", "blob", object])?;
        #[cfg(test)]
        record_test_blob_bytes(repo, contents.stdout.len() as u64);
        if hex(&Sha256::digest(&contents.stdout)) != sha {
            return Err(FleetError::Invalid(
                "Git blob SHA-256 differs from its content address".into(),
            ));
        }
        found.insert(sha.to_owned());
    }
    if found.len() != declared.len() || declared.keys().any(|sha| !found.contains(sha)) {
        return Err(FleetError::Invalid(
            "blob metadata and raw Git blob files do not match".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
fn test_blob_byte_counts() -> &'static std::sync::Mutex<BTreeMap<PathBuf, u64>> {
    use std::sync::{Mutex, OnceLock};
    static COUNTS: OnceLock<Mutex<BTreeMap<PathBuf, u64>>> = OnceLock::new();
    COUNTS.get_or_init(Default::default)
}

#[cfg(test)]
fn record_test_blob_bytes(repo: &Path, bytes: u64) {
    let Ok(repo) = fs::canonicalize(repo) else {
        return;
    };
    let mut counts = test_blob_byte_counts().lock().unwrap();
    *counts.entry(repo).or_default() += bytes;
}

#[cfg(test)]
fn test_blob_bytes_validated(repo: &Path, reset: bool) -> u64 {
    let Ok(repo) = fs::canonicalize(repo) else {
        return 0;
    };
    let mut counts = test_blob_byte_counts().lock().unwrap();
    if reset {
        return counts.remove(&repo).unwrap_or_default();
    }
    counts.get(&repo).copied().unwrap_or_default()
}

fn validate_projection_ownership(
    repo: &Path,
    base: &str,
    incoming: &str,
    source_node_id: &str,
) -> Result<()> {
    let base = records_at_tree(repo, base)?;
    let incoming = records_at_tree(repo, incoming)?;
    let before = base
        .into_iter()
        .map(|r| (r.entity_id(), r))
        .collect::<BTreeMap<_, _>>();
    let after = incoming
        .into_iter()
        .map(|r| (r.entity_id(), r))
        .collect::<BTreeMap<_, _>>();
    for (identity, _old) in &before {
        if after.contains_key(identity) {
            continue;
        }
        if after.keys().any(|new| new.id == identity.id) {
            return Err(FleetError::Invalid(format!(
                "record origin identity cannot change during an edit: {}",
                identity.id
            )));
        }
    }
    let identities = before
        .keys()
        .chain(after.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for identity in identities {
        let old = before.get(&identity);
        let new = after.get(&identity);
        if old == new {
            continue;
        }
        let projection =
            old.is_some_and(is_execution_projection) || new.is_some_and(is_execution_projection);
        if !projection {
            continue;
        }
        let Some(new) = new else {
            return Err(FleetError::Invalid(format!(
                "execution projections must be tombstoned, not removed: {}",
                identity.id
            )));
        };
        if new.origin_node_id != source_node_id
            || old.is_some_and(|old| old.origin_node_id != source_node_id)
        {
            return Err(FleetError::Invalid(format!(
                "node {source_node_id} cannot publish execution projection {} owned by {}",
                new.id, new.origin_node_id
            )));
        }
    }
    Ok(())
}

fn is_execution_projection(record: &Record) -> bool {
    matches!(
        record.kind,
        RecordKind::Blob | RecordKind::Transcript | RecordKind::Task | RecordKind::Workflow
    )
}

fn git_config(repo: &Path, key: &str) -> Result<String> {
    let output = git(repo, ["config", "--get", key])?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn ensure_fleet_identity(repo: &Path, node_id: &str, primary: bool, origin: &str) -> Result<()> {
    let expected_role = if primary { "true" } else { "false" };
    let stored_node = git_config_optional(repo, "fleet.nodeId")?;
    let stored_role = git_config_optional(repo, "fleet.primary")?;
    let stored_origin = git_config_optional(repo, "fleet.origin")?;
    if stored_node.is_none() && stored_role.is_none() && stored_origin.is_none() {
        let branch = git(repo, ["branch", "--show-current"])?;
        let branch = String::from_utf8_lossy(&branch.stdout).trim().to_owned();
        let has_commit = git(repo, ["rev-parse", "--verify", "HEAD"]).is_ok();
        let remotes = git(repo, ["remote"])?;
        if has_commit || branch != "main" || !remotes.stdout.is_empty() {
            return Err(FleetError::Invalid(
                "existing Git repository has no fleet identity and cannot be adopted".into(),
            ));
        }
    } else {
        if stored_node.as_deref().is_some_and(|value| value != node_id)
            || stored_role
                .as_deref()
                .is_some_and(|value| value != expected_role)
            || stored_origin
                .as_deref()
                .is_some_and(|value| value != origin)
        {
            return Err(FleetError::Invalid(
                "existing Git repository fleet identity does not match requested role, node, and origin"
                    .into(),
            ));
        }
        let complete = stored_node.is_some() && stored_role.is_some() && stored_origin.is_some();
        if !complete && git(repo, ["rev-parse", "--verify", "HEAD"]).is_ok() {
            return Err(FleetError::Invalid(
                "committed Git repository has an incomplete fleet identity".into(),
            ));
        }
    }
    git(repo, ["config", "fleet.nodeId", node_id])?;
    git(repo, ["config", "fleet.primary", expected_role])?;
    git(repo, ["config", "fleet.origin", origin])?;
    Ok(())
}

fn git_config_optional(repo: &Path, key: &str) -> Result<Option<String>> {
    match git(repo, ["config", "--get", key]) {
        Ok(output) => Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        )),
        Err(FleetError::Git { .. }) => Ok(None),
        Err(error) => Err(error),
    }
}

fn ensure_origin_remote(repo: &Path, expected: &str) -> Result<()> {
    match git(repo, ["remote", "get-url", "origin"]) {
        Ok(output) if String::from_utf8_lossy(&output.stdout).trim() == expected => Ok(()),
        Ok(_) => Err(FleetError::Invalid(
            "existing origin remote does not match requested fleet origin".into(),
        )),
        Err(_) => {
            git(repo, ["remote", "add", "origin", expected])?;
            Ok(())
        }
    }
}

fn ensure_branch(repo: &Path, expected: &str) -> Result<()> {
    let branch = git(repo, ["branch", "--show-current"])?;
    let branch = String::from_utf8_lossy(&branch.stdout).trim().to_owned();
    if branch != expected {
        return Err(FleetError::Invalid(format!(
            "existing checkout branch {branch} does not match requested {expected}"
        )));
    }
    Ok(())
}

fn ensure_node_branch(repo: &Path, node_id: &str) -> Result<()> {
    let expected = format!("nodes/{node_id}");
    let branch = git(repo, ["branch", "--show-current"])?;
    let branch = String::from_utf8_lossy(&branch.stdout).trim().to_owned();
    if branch == expected {
        return Ok(());
    }
    if branch != "main" || git(repo, ["rev-parse", "--verify", "HEAD"]).is_ok() {
        return Err(FleetError::Invalid(format!(
            "existing node checkout is on unexpected branch {branch}"
        )));
    }
    git(repo, ["checkout", "-b", &expected, "origin/main"])?;
    Ok(())
}

fn ensure_node_push(
    repo: &Path,
    node_id: &str,
    transport: Option<&Arc<dyn GitTransfer>>,
) -> Result<()> {
    let branch = format!("nodes/{node_id}");
    let remote_ref = format!("refs/remotes/origin/{branch}");
    let local = git(repo, ["rev-parse", "HEAD"])?;
    match git(repo, ["rev-parse", "--verify", &remote_ref]) {
        Ok(remote) if remote.stdout == local.stdout => return Ok(()),
        Ok(remote) => {
            let remote = String::from_utf8_lossy(&remote.stdout).trim().to_owned();
            let local = String::from_utf8_lossy(&local.stdout).trim().to_owned();
            if git(repo, ["merge-base", "--is-ancestor", &remote, &local]).is_err() {
                return Err(FleetError::Invalid(format!(
                    "remote branch {branch} is not an ancestor of the local checkout"
                )));
            }
            if remote == local {
                return Ok(());
            }
        }
        Err(_) => {}
    }
    match transport {
        Some(transport) => transport
            .push(repo, &branch)
            .map_err(FleetError::Transfer)?,
        None => {
            git(repo, ["push", "-u", "origin", &branch])?;
        }
    }
    Ok(())
}

fn configure_repo(repo: &Path, node_id: &str, meta_root: &Path) -> Result<()> {
    let hooks = meta_root.join("no-hooks");
    git(repo, ["config", "user.name", "Brr Fleet"])?;
    git(repo, ["config", "user.email", "brr-fleet@localhost"])?;
    git(
        repo,
        [
            "config",
            "core.hooksPath",
            hooks
                .to_str()
                .ok_or_else(|| FleetError::Invalid("non-UTF8 hooks path".into()))?,
        ],
    )?;
    git(repo, ["config", "core.autocrlf", "false"])?;
    git(repo, ["config", "fleet.nodeId", node_id])?;
    Ok(())
}

fn git<I, S>(cwd: &Path, args: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let args = args
        .into_iter()
        .map(|a| a.as_ref().to_os_string())
        .collect::<Vec<_>>();
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(&args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()?;
    if !output.status.success() {
        return Err(FleetError::Git {
            args: args
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(output)
}

fn git_index_path(repo: &Path) -> Result<PathBuf> {
    let output = git(repo, ["rev-parse", "--git-path", "index"])?;
    let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    Ok(if path.is_absolute() {
        path
    } else {
        repo.join(path)
    })
}

fn restore_optional_file(path: &Path, contents: Option<&[u8]>) -> Result<()> {
    match contents {
        Some(contents) => fs::write(path, contents).map_err(FleetError::from),
        None => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        },
    }
}

fn unmerged_paths(repo: &Path) -> Result<Vec<String>> {
    let out = git(repo, ["diff", "--name-only", "--diff-filter=U"])?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect())
}

fn staged_paths(repo: &Path) -> Result<Vec<String>> {
    let output = git(repo, ["diff", "--cached", "--name-only", "-z"])?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect())
}

fn unmerged_record_ids(repo: &Path) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    for path in unmerged_paths(repo)? {
        if let Some(record) = read_index_record(repo, 2, &path)?
            .or(read_index_record(repo, 3, &path)?)
            .or(read_index_record(repo, 1, &path)?)
        {
            ids.push(record.id);
        }
    }
    Ok(ids)
}

/// Resolve only JSON records whose independent field edits have a unique
/// three-way result. Overlapping scalar edits remain for explicit user choice.
fn auto_merge_conflicts(repo: &Path) -> Result<bool> {
    let mut resolutions = Vec::new();
    let paths = unmerged_paths(repo)?;
    if paths.is_empty() {
        return Ok(false);
    }
    for path in paths {
        let (Some(base), Some(current), Some(incoming)) = (
            read_index_record(repo, 1, &path)?,
            read_index_record(repo, 2, &path)?,
            read_index_record(repo, 3, &path)?,
        ) else {
            return Ok(false);
        };
        let Some(mut merged) = merge_records(&base, &current, &incoming) else {
            return Ok(false);
        };
        merged.revision = record_digest(&merged)?;
        resolutions.push((path, merged));
    }
    for (path, record) in resolutions {
        fs::write(repo.join(&path), serde_json::to_vec_pretty(&record)?)?;
        git(repo, ["add", "--", path.as_str()])?;
    }
    Ok(unmerged_paths(repo)?.is_empty())
}

fn merge_records(base: &Record, current: &Record, incoming: &Record) -> Option<Record> {
    if base.id != current.id
        || base.id != incoming.id
        || base.kind != current.kind
        || base.kind != incoming.kind
        || base.origin_node_id != current.origin_node_id
        || base.origin_node_id != incoming.origin_node_id
        || base.schema_version != current.schema_version
        || base.schema_version != incoming.schema_version
    {
        return None;
    }
    if (current.deleted && incoming.content != base.content)
        || (incoming.deleted && current.content != base.content)
    {
        return None;
    }
    let mut merged = current.clone();
    merged.origin_node_id = base.origin_node_id.clone();
    merged.base_revision = merge_optional(
        &base.base_revision,
        &current.base_revision,
        &incoming.base_revision,
    )?;
    merged.deleted = merge_scalar(&base.deleted, &current.deleted, &incoming.deleted)?;
    merged.content = merge_json(&base.content, &current.content, &incoming.content)?;
    Some(merged)
}

fn merge_scalar<T: Clone + PartialEq>(base: &T, current: &T, incoming: &T) -> Option<T> {
    if current == base {
        Some(incoming.clone())
    } else if incoming == base || current == incoming {
        Some(current.clone())
    } else {
        None
    }
}

fn merge_optional<T: Clone + PartialEq>(
    base: &Option<T>,
    current: &Option<T>,
    incoming: &Option<T>,
) -> Option<Option<T>> {
    merge_scalar(base, current, incoming)
}

fn merge_json(base: &Value, current: &Value, incoming: &Value) -> Option<Value> {
    if current == base {
        return Some(incoming.clone());
    }
    if incoming == base || current == incoming {
        return Some(current.clone());
    }
    if let (Value::Object(base), Value::Object(current), Value::Object(incoming)) =
        (base, current, incoming)
    {
        let keys = base
            .keys()
            .chain(current.keys())
            .chain(incoming.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut result = serde_json::Map::new();
        for key in keys {
            match merge_json_field(base.get(&key), current.get(&key), incoming.get(&key))? {
                Some(value) => {
                    result.insert(key, value);
                }
                None => {}
            }
        }
        return Some(Value::Object(result));
    }
    None
}

fn merge_json_field(
    base: Option<&Value>,
    current: Option<&Value>,
    incoming: Option<&Value>,
) -> Option<Option<Value>> {
    if current == base {
        return Some(incoming.cloned());
    }
    if incoming == base || current == incoming {
        return Some(current.cloned());
    }
    match (base, current, incoming) {
        (Some(base), Some(current), Some(incoming)) => {
            merge_json(base, current, incoming).map(Some)
        }
        _ => None,
    }
}

fn read_index_record(repo: &Path, stage: u8, path: &str) -> Result<Option<Record>> {
    let spec = format!(":{stage}:{path}");
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["show", spec.as_str()])
        .output()?;
    if !output.status.success() {
        return Ok(None);
    }
    let record: Record = serde_json::from_slice(&output.stdout)?;
    validate_record(&record)?;
    Ok(Some(record))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        process::Command,
        sync::atomic::{AtomicUsize, Ordering},
        sync::{Arc, Mutex},
    };

    use serde_json::json;
    use sha2::{Digest, Sha256};

    use super::{
        BlobFile, FleetRepo, GitTransfer, Record, RecordKind, git, hex, record_path,
        test_blob_bytes_validated,
    };

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    #[derive(Default)]
    struct LocalTransfer {
        origin: PathBuf,
        calls: Mutex<Vec<(String, PathBuf, String)>>,
        failed_fetches: AtomicUsize,
        empty_fetches: AtomicUsize,
        failed_pushes: AtomicUsize,
    }

    impl LocalTransfer {
        fn new(origin: impl Into<PathBuf>) -> Self {
            Self {
                origin: origin.into(),
                calls: Mutex::new(Vec::new()),
                failed_fetches: AtomicUsize::new(0),
                empty_fetches: AtomicUsize::new(0),
                failed_pushes: AtomicUsize::new(0),
            }
        }

        fn failing_first_push(origin: impl Into<PathBuf>) -> Self {
            Self {
                origin: origin.into(),
                calls: Mutex::new(Vec::new()),
                failed_fetches: AtomicUsize::new(0),
                empty_fetches: AtomicUsize::new(0),
                failed_pushes: AtomicUsize::new(1),
            }
        }

        fn failing_first_fetch(origin: impl Into<PathBuf>) -> Self {
            Self {
                origin: origin.into(),
                calls: Mutex::new(Vec::new()),
                failed_fetches: AtomicUsize::new(1),
                empty_fetches: AtomicUsize::new(0),
                failed_pushes: AtomicUsize::new(0),
            }
        }

        fn empty_first_fetch(origin: impl Into<PathBuf>) -> Self {
            Self {
                origin: origin.into(),
                calls: Mutex::new(Vec::new()),
                failed_fetches: AtomicUsize::new(0),
                empty_fetches: AtomicUsize::new(1),
                failed_pushes: AtomicUsize::new(0),
            }
        }
    }

    impl GitTransfer for LocalTransfer {
        fn fetch(&self, repo: &std::path::Path) -> std::result::Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(("fetch".into(), repo.to_path_buf(), String::new()));
            if self
                .failed_fetches
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
            {
                return Err("injected fetch interruption".into());
            }
            if self
                .empty_fetches
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
            {
                return Ok(());
            }
            git(
                repo,
                [
                    "fetch",
                    self.origin.to_str().unwrap(),
                    "+refs/heads/*:refs/remotes/origin/*",
                ],
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
        }

        fn push(&self, repo: &std::path::Path, branch: &str) -> std::result::Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(("push".into(), repo.to_path_buf(), branch.to_owned()));
            if self
                .failed_pushes
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
            {
                return Err("injected push interruption".into());
            }
            git(
                repo,
                [
                    "push",
                    self.origin.to_str().unwrap(),
                    &format!("HEAD:refs/heads/{branch}"),
                ],
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
        }
    }

    fn temp_root(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "brr-fleet-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn approved_primary(root: impl AsRef<std::path::Path>, node_id: &str) -> FleetRepo {
        let mut repo = FleetRepo::init_primary(root, node_id).unwrap();
        repo.set_domain_validator(|_| Ok(()));
        repo
    }

    fn record(node: &str) -> Record {
        Record {
            schema_version: 1,
            id: "agent:ada".into(),
            kind: RecordKind::Agent,
            origin_node_id: node.into(),
            revision: String::new(),
            base_revision: None,
            deleted: false,
            content: json!({"name":"Ada","instructions":"Be precise"}),
        }
    }

    #[test]
    fn repeated_reads_of_same_tree_do_not_rehash_committed_blob_bytes() {
        let root = temp_root("validated-tree-cache");
        let mut primary = approved_primary(root.join("primary"), "primary");
        let bytes = vec![0x5a; 4096];
        let source = root.join("source.bin");
        fs::write(&source, &bytes).unwrap();
        let sha = hex(&Sha256::digest(&bytes));
        let blob = Record {
            schema_version: 1,
            id: sha.clone(),
            kind: RecordKind::Blob,
            origin_node_id: "primary".into(),
            revision: String::new(),
            base_revision: None,
            deleted: false,
            content: json!({"sha256":sha,"sizeBytes":bytes.len()}),
        };
        primary
            .commit_records_with_blobs(
                vec![(blob, None)],
                vec![BlobFile {
                    sha256: sha,
                    size_bytes: bytes.len() as u64,
                    source_path: fs::canonicalize(source).unwrap(),
                }],
            )
            .unwrap();

        let primary_path = primary.data_root().to_path_buf();
        drop(primary);
        let primary = FleetRepo::open_primary(primary_path, "primary").unwrap();
        test_blob_bytes_validated(&primary.repo, true);
        assert_eq!(primary.list_records().unwrap().len(), 1);
        let first_read_bytes = test_blob_bytes_validated(&primary.repo, false);
        assert_eq!(first_read_bytes, bytes.len() as u64);

        assert_eq!(primary.list_records().unwrap().len(), 1);
        assert_eq!(
            test_blob_bytes_validated(&primary.repo, false),
            first_read_bytes,
            "a repeated read of an unchanged validated Git tree should not hash its raw blobs again"
        );
    }

    #[test]
    fn a_new_tree_with_a_corrupt_content_addressed_blob_is_still_rejected() {
        let root = temp_root("validated-tree-cache-new-corruption");
        let mut primary = approved_primary(root.join("primary"), "primary");
        let bytes = b"good";
        let source = root.join("source.bin");
        fs::write(&source, bytes).unwrap();
        let sha = hex(&Sha256::digest(bytes));
        let blob = Record {
            schema_version: 1,
            id: sha.clone(),
            kind: RecordKind::Blob,
            origin_node_id: "primary".into(),
            revision: String::new(),
            base_revision: None,
            deleted: false,
            content: json!({"sha256":sha,"sizeBytes":bytes.len()}),
        };
        primary
            .commit_records_with_blobs(
                vec![(blob, None)],
                vec![BlobFile {
                    sha256: sha.clone(),
                    size_bytes: bytes.len() as u64,
                    source_path: fs::canonicalize(source).unwrap(),
                }],
            )
            .unwrap();
        assert_eq!(primary.list_records().unwrap().len(), 1);

        fs::write(primary.repo.join("blobs").join(&sha), b"evil").unwrap();
        git(&primary.repo, ["add", &format!("blobs/{sha}")]).unwrap();
        git(
            &primary.repo,
            ["commit", "-m", "Tamper with a new tree fixture"],
        )
        .unwrap();
        test_blob_bytes_validated(&primary.repo, true);

        let error = primary.list_records().unwrap_err().to_string();
        assert!(error.contains("SHA-256 differs from its content address"));
        assert_eq!(
            test_blob_bytes_validated(&primary.repo, false),
            bytes.len() as u64
        );
    }

    #[test]
    fn a_new_tree_with_a_corrupt_record_revision_is_not_served_from_old_cache() {
        let root = temp_root("validated-tree-cache-record-revision");
        let mut primary = approved_primary(root.join("primary"), "primary");
        primary.commit_record(record("primary"), None).unwrap();
        assert_eq!(primary.list_records().unwrap().len(), 1);

        let path = record_path(&record("primary"));
        let record_file = primary.repo.join(&path);
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&record_file).unwrap()).unwrap();
        value["revision"] = json!("not-the-record-digest");
        fs::write(&record_file, serde_json::to_vec(&value).unwrap()).unwrap();
        git(&primary.repo, ["add", &path]).unwrap();
        git(
            &primary.repo,
            ["commit", "-m", "Corrupt record revision fixture"],
        )
        .unwrap();

        let error = primary.list_records().unwrap_err().to_string();
        assert!(error.contains("record revision mismatch"));
    }

    #[test]
    fn a_new_tombstone_tree_replaces_cached_owner_records() {
        let root = temp_root("validated-tree-cache-tombstone");
        let mut primary = approved_primary(root.join("primary"), "primary");
        let saved = primary.commit_record(record("primary"), None).unwrap();
        assert!(!primary.list_records().unwrap()[0].deleted);

        let mut tombstone = saved.clone();
        tombstone.deleted = true;
        tombstone.revision.clear();
        tombstone.base_revision = None;
        primary
            .commit_record(tombstone, Some(&saved.revision))
            .unwrap();

        let current = primary.list_records().unwrap();
        assert_eq!(current.len(), 1);
        assert!(current[0].deleted);
        assert_eq!(current[0].origin_node_id, "primary");
    }

    #[test]
    fn blob_metadata_requires_atomic_raw_file_input() {
        let sha = hex(&Sha256::digest(b"hello"));
        let blob = Record {
            schema_version: 1,
            id: sha.clone(),
            kind: RecordKind::Blob,
            origin_node_id: "primary".into(),
            revision: String::new(),
            base_revision: None,
            deleted: false,
            content: json!({"sha256":sha,"sizeBytes":5}),
        };
        let root = temp_root("content-addressed-blob");
        let mut primary = approved_primary(root.join("primary"), "primary");
        let before = primary.head().unwrap();
        let error = primary.commit_record(blob, None).unwrap_err().to_string();
        assert!(error.contains("requires its source file in the same batch"));
        assert_eq!(primary.head().unwrap(), before);
        assert!(primary.list_records().unwrap().is_empty());
    }

    #[test]
    fn injected_transport_handles_initial_sync_staging_and_pending_resolution() {
        let root = temp_root("injected-transfer");
        let mut primary = approved_primary(root.join("primary"), "primary");
        let local_origin = primary.origin().to_owned();
        let transport = Arc::new(LocalTransfer::new(&local_origin));
        let transport_trait: Arc<dyn GitTransfer> = transport.clone();
        let node_path = root.join("node");
        let pinned_url = "https://127.0.0.1:1/pinned-origin.git";
        let mut node = FleetRepo::init_node_with_transport(
            &node_path,
            "laptop",
            pinned_url,
            transport_trait.clone(),
        )
        .unwrap();

        let repo = node.repo_path().to_path_buf();
        let initial = transport.calls.lock().unwrap().clone();
        assert_eq!(initial.len(), 2);
        assert_eq!(initial[0].0, "fetch");
        assert_eq!(initial[0].1, repo);
        assert_eq!(initial[1].0, "push");
        assert_eq!(initial[1].2, "nodes/laptop");
        assert_eq!(node.origin(), pinned_url);

        let base = node.commit_record(record("laptop"), None).unwrap();
        node.sync().unwrap();
        assert!(
            transport
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|(kind, path, _)| { kind == "fetch" && path.ends_with("merge-staging") })
        );
        primary.sync().unwrap();

        let mut primary_edit = base.clone();
        primary_edit.content["instructions"] = serde_json::json!("Primary transport edit");
        primary
            .commit_record(primary_edit, Some(&base.revision))
            .unwrap();
        primary.sync().unwrap();
        let mut node_edit = base.clone();
        node_edit.content["instructions"] = serde_json::json!("Node transport edit");
        node.commit_record(node_edit, Some(&base.revision)).unwrap();
        assert!(matches!(
            node.sync().unwrap(),
            super::SyncOutcome::Conflicted { .. }
        ));
        let conflict = node.conflicts().unwrap().pop().unwrap();
        let calls_before_resolution = transport.calls.lock().unwrap().len();
        node.resolve("agent:ada", conflict.current.unwrap())
            .unwrap();
        let calls = transport.calls.lock().unwrap().clone();
        assert!(calls.len() > calls_before_resolution);
        assert_eq!(calls.last().unwrap().0, "push");
        assert_eq!(calls.last().unwrap().2, "nodes/laptop");
        assert!(
            calls
                .iter()
                .any(|(kind, _, branch)| { kind == "push" && branch == "nodes/laptop" })
        );

        drop(node);
        let mut reopened =
            FleetRepo::open_with_transport(&node_path, "laptop", transport_trait).unwrap();
        reopened.sync().unwrap();
        assert!(reopened.list_records().unwrap().iter().any(|record| {
            record.content["instructions"] == "Node transport edit"
                || record.content["instructions"] == "Primary transport edit"
        }));
    }

    #[test]
    fn interrupted_node_activation_resumes_and_preserves_local_commit() {
        let root = temp_root("resume-node-activation");
        let primary = approved_primary(root.join("primary"), "primary");
        let origin = primary.origin().to_owned();
        let pinned_url = "https://127.0.0.1:1/fleet.git";
        let transport = Arc::new(LocalTransfer::failing_first_push(&origin));
        let transport_trait: Arc<dyn GitTransfer> = transport.clone();
        let node_path = root.join("node");

        assert!(
            FleetRepo::init_node_with_transport(
                &node_path,
                "laptop",
                pinned_url,
                transport_trait.clone(),
            )
            .is_err()
        );

        // A local edit can happen after the push error and before activation
        // resumes. Retrying must publish that commit instead of resetting it
        // to origin/main.
        let mut partial =
            FleetRepo::open_with_transport(&node_path, "laptop", transport_trait.clone()).unwrap();
        partial.set_domain_validator(|_| Ok(()));
        let saved = partial.commit_record(record("laptop"), None).unwrap();
        let before_resume = partial.head().unwrap();
        drop(partial);

        let resumed = FleetRepo::init_node_with_transport(
            &node_path,
            "laptop",
            pinned_url,
            transport_trait.clone(),
        )
        .unwrap();
        assert_eq!(resumed.head().unwrap(), before_resume);
        assert_eq!(resumed.list_records().unwrap(), vec![saved]);
        assert!(
            FleetRepo::init_node_with_transport(
                &node_path,
                "other-node",
                pinned_url,
                transport_trait.clone(),
            )
            .is_err()
        );
        assert!(
            FleetRepo::init_node_with_transport(
                &node_path,
                "laptop",
                "https://127.0.0.1:2/wrong-origin.git",
                transport_trait,
            )
            .is_err()
        );
        assert_eq!(
            FleetRepo::open_with_transport(
                &node_path,
                "laptop",
                Arc::new(LocalTransfer::new(&origin))
            )
            .unwrap()
            .head()
            .unwrap(),
            before_resume
        );
        assert_eq!(
            git(
                std::path::Path::new(&origin),
                ["rev-parse", "refs/heads/nodes/laptop"]
            )
            .unwrap()
            .stdout,
            format!("{before_resume}\n").as_bytes()
        );
    }

    #[test]
    fn interrupted_node_fetch_resumes_from_existing_git_directory() {
        let root = temp_root("resume-node-fetch");
        let primary = approved_primary(root.join("primary"), "primary");
        let origin = primary.origin().to_owned();
        let pinned_url = "https://127.0.0.1:1/fleet.git";
        let transport = Arc::new(LocalTransfer::failing_first_fetch(&origin));
        let transport_trait: Arc<dyn GitTransfer> = transport.clone();
        let node_path = root.join("node");

        assert!(
            FleetRepo::init_node_with_transport(
                &node_path,
                "laptop",
                pinned_url,
                transport_trait.clone(),
            )
            .is_err()
        );
        let resumed =
            FleetRepo::init_node_with_transport(&node_path, "laptop", pinned_url, transport_trait)
                .unwrap();
        assert_eq!(resumed.origin(), pinned_url);
        assert!(resumed.head().is_ok());
    }

    #[test]
    fn checkout_failure_after_empty_fetch_can_resume_safely() {
        let root = temp_root("resume-node-checkout");
        let primary = approved_primary(root.join("primary"), "primary");
        let origin = primary.origin().to_owned();
        let pinned_url = "https://127.0.0.1:1/fleet.git";
        let transport = Arc::new(LocalTransfer::empty_first_fetch(&origin));
        let transport_trait: Arc<dyn GitTransfer> = transport.clone();
        let node_path = root.join("node");

        assert!(
            FleetRepo::init_node_with_transport(
                &node_path,
                "laptop",
                pinned_url,
                transport_trait.clone(),
            )
            .is_err()
        );
        let resumed =
            FleetRepo::init_node_with_transport(&node_path, "laptop", pinned_url, transport_trait)
                .unwrap();
        assert_eq!(resumed.origin(), pinned_url);
        assert_eq!(resumed.head().unwrap(), primary.head().unwrap());
    }

    #[test]
    fn primary_and_node_use_real_git_and_enforce_record_revisions() {
        let root = temp_root("commit");
        let primary_path = root.join("primary");
        let node_path = root.join("node");
        let primary = approved_primary(&primary_path, "primary");
        let origin = primary.origin().to_owned();
        let mut node = FleetRepo::init_node(&node_path, "laptop", &origin).unwrap();

        let saved = node.commit_record(record("laptop"), None).unwrap();
        let reopened = FleetRepo::open_node(&node_path, "laptop", &origin).unwrap();
        assert_eq!(reopened.head().unwrap(), node.head().unwrap());
        assert_eq!(reopened.origin(), origin);
        assert_eq!(
            FleetRepo::open_primary(&primary_path, "primary")
                .unwrap()
                .head()
                .unwrap(),
            primary.head().unwrap()
        );
        assert!(FleetRepo::open_node(&node_path, "wrong-node", &origin).is_err());
        assert!(FleetRepo::open_node(&node_path, "laptop", "wrong-origin").is_err());
        assert!(FleetRepo::open(&primary_path, "primary").unwrap().primary);
        assert_eq!(saved.revision.len(), 64);
        assert!(node.commit_record(record("laptop"), None).is_err());
        let mut updated = saved.clone();
        updated.content["instructions"] = json!("Be exact");
        assert!(
            node.commit_record(updated.clone(), Some("stale-revision"))
                .is_err()
        );
        let updated = node.commit_record(updated, Some(&saved.revision)).unwrap();
        let cli_head = Command::new("git")
            .args([
                "-C",
                node.repo_path().to_str().unwrap(),
                "rev-parse",
                "HEAD",
            ])
            .output()
            .unwrap();
        assert!(cli_head.status.success());
        assert_eq!(String::from_utf8_lossy(&cli_head.stdout).trim().len(), 40);
        assert_eq!(node.list_records().unwrap(), vec![updated]);
        Command::new("git")
            .args([
                "-C",
                node.repo_path().to_str().unwrap(),
                "checkout",
                "-b",
                "nodes/wrong",
            ])
            .status()
            .unwrap();
        assert!(FleetRepo::open_node(&node_path, "laptop", &origin).is_err());
    }

    #[test]
    fn primary_activation_retry_preserves_existing_local_and_origin_commits() {
        let root = temp_root("resume-primary-activation");
        let mut primary = approved_primary(&root, "primary");
        let record = primary.commit_record(record("primary"), None).unwrap();
        primary.sync().unwrap();
        let head = primary.head().unwrap();
        drop(primary);

        let resumed = FleetRepo::init_primary(&root, "primary").unwrap();
        assert_eq!(resumed.head().unwrap(), head);
        assert_eq!(resumed.list_records().unwrap(), vec![record]);
        let origin_head = git(
            std::path::Path::new(resumed.origin()),
            ["rev-parse", "refs/heads/main"],
        )
        .unwrap();
        assert_eq!(String::from_utf8_lossy(&origin_head.stdout).trim(), head);
    }

    #[test]
    fn batch_records_share_one_real_git_commit_and_return_new_revisions() {
        let root = temp_root("batch-one-commit");
        let mut primary = approved_primary(root.join("primary"), "primary");
        let before = primary.head().unwrap();
        let mut first = record("primary");
        first.id = "agent:ada".into();
        let mut second = record("primary");
        second.id = "memory:fact-1".into();
        second.kind = RecordKind::Memory;

        let saved = primary
            .commit_records(vec![(first, None), (second, None)])
            .unwrap();

        assert_eq!(saved.len(), 2);
        assert_eq!(primary.list_records().unwrap(), saved);
        assert_ne!(primary.head().unwrap(), before);
        let parents = Command::new("git")
            .args([
                "-C",
                primary.repo_path().to_str().unwrap(),
                "rev-list",
                "--parents",
                "-n",
                "1",
                "HEAD",
            ])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&parents.stdout)
                .split_whitespace()
                .count(),
            2
        );
        let changed = Command::new("git")
            .args([
                "-C",
                primary.repo_path().to_str().unwrap(),
                "diff-tree",
                "--no-commit-id",
                "--name-only",
                "-r",
                "HEAD",
            ])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&changed.stdout).lines().count(), 2);
    }

    #[test]
    fn stale_revision_in_a_batch_leaves_head_index_and_working_tree_unchanged() {
        let root = temp_root("batch-stale");
        let mut primary = approved_primary(root.join("primary"), "primary");
        let saved = primary.commit_record(record("primary"), None).unwrap();
        let before = primary.head().unwrap();
        let mut update = saved.clone();
        update.content["instructions"] = json!("new instructions");
        let mut new_memory = record("primary");
        new_memory.id = "memory:fact-1".into();
        new_memory.kind = RecordKind::Memory;

        let error = primary
            .commit_records(vec![
                (update, Some(saved.revision.clone())),
                (new_memory, Some("stale-revision".into())),
            ])
            .unwrap_err();

        assert!(matches!(error, super::FleetError::RevisionConflict { .. }));
        assert_eq!(primary.head().unwrap(), before);
        assert_eq!(primary.list_records().unwrap(), vec![saved]);
        assert!(git(&primary.repo, ["diff", "--cached", "--quiet"]).is_ok());
        assert!(
            git(&primary.repo, ["status", "--porcelain"])
                .unwrap()
                .stdout
                .is_empty()
        );
    }

    #[test]
    fn domain_rejection_restores_tracked_files_and_rolls_back_the_entire_batch() {
        let root = temp_root("batch-domain-rollback");
        let mut primary = FleetRepo::init_primary(root.join("primary"), "primary").unwrap();
        primary.set_domain_validator(|records| {
            if records.iter().all(|record| record.content["valid"] == true) {
                Ok(())
            } else {
                Err("record was not reviewed".into())
            }
        });
        let mut initial = record("primary");
        initial.content["valid"] = json!(true);
        let saved = primary.commit_record(initial, None).unwrap();
        let before = primary.head().unwrap();
        let saved_path = primary.repo.join(record_path(&saved));
        let before_bytes = fs::read(&saved_path).unwrap();

        let mut rejected = saved.clone();
        rejected.content["valid"] = json!(false);
        let mut second = record("primary");
        second.id = "agent:grace".into();
        second.content["valid"] = json!(true);
        let second_path = primary.repo.join(record_path(&second));
        let result = primary.commit_records(vec![
            (rejected, Some(saved.revision.clone())),
            (second, None),
        ]);

        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("domain validation failed")
        );
        assert_eq!(primary.head().unwrap(), before);
        assert_eq!(primary.list_records().unwrap(), vec![saved.clone()]);
        assert_eq!(fs::read(&saved_path).unwrap(), before_bytes);
        assert!(
            git(&primary.repo, ["status", "--porcelain"])
                .unwrap()
                .stdout
                .is_empty()
        );
        assert!(!second_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn batch_refuses_a_preexisting_symlink_at_a_record_path() {
        use std::os::unix::fs::symlink;

        let root = temp_root("batch-symlink");
        let mut primary = approved_primary(root.join("primary"), "primary");
        let mut candidate = record("primary");
        candidate.revision = super::record_digest(&candidate).unwrap();
        let target = primary.repo.join(record_path(&candidate));
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        let outside = root.join("outside.json");
        fs::write(&outside, b"do not overwrite").unwrap();
        symlink(&outside, &target).unwrap();
        let before = primary.head().unwrap();

        assert!(primary.commit_records(vec![(candidate, None)]).is_err());

        assert_eq!(fs::read(&outside).unwrap(), b"do not overwrite");
        assert!(
            fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(primary.head().unwrap(), before);
        assert!(git(&primary.repo, ["diff", "--cached", "--quiet"]).is_ok());
    }

    #[test]
    fn primary_does_not_publish_preexisting_staged_records_with_a_reviewed_write() {
        let root = temp_root("staged-before-commit");
        let mut primary = approved_primary(root.join("primary"), "primary");
        let hidden = record("laptop");
        let hidden_path = primary.repo.join(record_path(&hidden));
        fs::create_dir_all(hidden_path.parent().unwrap()).unwrap();
        fs::write(&hidden_path, serde_json::to_vec_pretty(&hidden).unwrap()).unwrap();
        git(&primary.repo, ["add", "--", record_path(&hidden).as_str()]).unwrap();

        let visible = record("primary");
        let previous_head = primary.head().unwrap();
        let result = primary.commit_record(visible, None);

        assert!(
            result.is_err(),
            "a pending index change must block commit_record"
        );
        assert_eq!(primary.head().unwrap(), previous_head);
        assert!(primary.list_records().unwrap().is_empty());
        assert!(git(&primary.repo, ["diff", "--cached", "--quiet"]).is_err());
    }

    #[test]
    fn primary_merges_a_published_offline_node_branch_into_main() {
        let root = temp_root("clean-merge");
        let primary_path = root.join("primary");
        let node_path = root.join("node");
        let mut primary = approved_primary(&primary_path, "primary");
        let origin = primary.origin().to_owned();
        let mut node = FleetRepo::init_node(&node_path, "laptop", &origin).unwrap();
        let saved = node.commit_record(record("laptop"), None).unwrap();
        assert!(matches!(
            node.sync().unwrap(),
            super::SyncOutcome::Published { .. }
        ));

        let outcome = primary.sync().unwrap();
        assert!(
            matches!(outcome, super::SyncOutcome::Merged { .. }),
            "{outcome:?}"
        );
        assert_eq!(primary.list_records().unwrap(), vec![saved]);
        assert!(
            Command::new("git")
                .args([
                    "--git-dir",
                    primary.origin(),
                    "show-ref",
                    "--verify",
                    "refs/heads/main"
                ])
                .output()
                .unwrap()
                .status
                .success()
        );
    }

    #[test]
    fn divergent_edits_preserve_main_until_manual_resolution_creates_merge_commit() {
        let root = temp_root("conflict");
        let primary_path = root.join("primary");
        let node_path = root.join("node");
        let mut primary = approved_primary(&primary_path, "primary");
        let origin = primary.origin().to_owned();
        let mut node = FleetRepo::init_node(&node_path, "laptop", &origin).unwrap();
        let base = node.commit_record(record("laptop"), None).unwrap();
        node.sync().unwrap();
        primary.sync().unwrap();
        let mut primary_edit = base.clone();
        primary_edit.content["instructions"] = json!("Primary local edit");
        primary
            .commit_record(primary_edit, Some(&base.revision))
            .unwrap();
        let valid_main = primary.head().unwrap();
        primary.sync().unwrap();
        let mut incoming = base.clone();
        incoming.content = json!({"name":"Ada","instructions":"Use concise examples"});
        node.commit_record(incoming, Some(&base.revision)).unwrap();
        node.sync().unwrap();

        let outcome = primary.sync().unwrap();
        assert!(
            matches!(outcome, super::SyncOutcome::Conflicted { .. }),
            "{outcome:?}"
        );
        let primary_path = primary.data_root().to_path_buf();
        primary = FleetRepo::open_primary(&primary_path, "primary").unwrap();
        primary.set_domain_validator(|_| Ok(()));
        assert_eq!(primary.list_records().unwrap().len(), 1);
        assert_eq!(primary.head().unwrap(), valid_main);
        let conflict = primary.conflicts().unwrap().pop().unwrap();
        assert_eq!(conflict.id, "agent:ada");
        assert_eq!(
            conflict.base.as_ref().unwrap().content["instructions"],
            "Be precise"
        );
        assert_eq!(
            conflict.current.as_ref().unwrap().content["instructions"],
            "Primary local edit"
        );
        assert_eq!(
            conflict.incoming.as_ref().unwrap().content["instructions"],
            "Use concise examples"
        );

        let mut resolution = conflict.current.clone().unwrap();
        resolution.content = json!({"name":"Ada","instructions":"Preserve reviewed guidance"});
        primary.resolve("agent:ada", resolution).unwrap();
        assert!(primary.conflicts().unwrap().is_empty());
        assert_eq!(
            primary.list_records().unwrap()[0].content["instructions"],
            "Preserve reviewed guidance"
        );
        let parents = Command::new("git")
            .args([
                "-C",
                primary.repo_path().to_str().unwrap(),
                "rev-list",
                "--parents",
                "-n",
                "1",
                "HEAD",
            ])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&parents.stdout)
                .split_whitespace()
                .count(),
            3
        );
    }

    #[test]
    fn rejected_final_resolution_restores_the_unmerged_index_and_worktree() {
        let root = temp_root("rejected-resolution");
        let mut primary = approved_primary(root.join("primary"), "primary");
        let origin = primary.origin().to_owned();
        let mut node = FleetRepo::init_node(root.join("node"), "laptop", &origin).unwrap();
        let base = node.commit_record(record("laptop"), None).unwrap();
        node.sync().unwrap();
        primary.sync().unwrap();
        let mut local = base.clone();
        local.content["instructions"] = json!("Primary edit");
        primary.commit_record(local, Some(&base.revision)).unwrap();
        primary.sync().unwrap();
        let mut incoming = base.clone();
        incoming.content["instructions"] = json!("Node edit");
        node.commit_record(incoming, Some(&base.revision)).unwrap();
        node.sync().unwrap();
        assert!(matches!(
            primary.sync().unwrap(),
            super::SyncOutcome::Conflicted { .. }
        ));

        primary.set_domain_validator(|records| {
            if records
                .iter()
                .any(|record| record.content["instructions"] == "rejected")
            {
                Err("test policy rejection".into())
            } else {
                Ok(())
            }
        });
        let conflict = primary.conflicts().unwrap().pop().unwrap();
        let resolution = conflict.current.clone().unwrap();
        let path = super::record_path(&resolution);
        let staged_file_before =
            fs::read(primary.meta_root.join("merge-conflict").join(&path)).unwrap();
        let index_path = Command::new("git")
            .args([
                "-C",
                primary.meta_root.join("merge-conflict").to_str().unwrap(),
                "rev-parse",
                "--git-path",
                "index",
            ])
            .output()
            .unwrap();
        let index_name = String::from_utf8_lossy(&index_path.stdout)
            .trim()
            .to_owned();
        let index_path = primary.meta_root.join("merge-conflict").join(index_name);
        let index_before = fs::read(&index_path).unwrap();
        let marker = primary.meta_root.join("resolution.pending");
        assert!(!marker.exists());

        let mut invalid = resolution.clone();
        invalid.content["instructions"] = json!("rejected");
        assert!(primary.resolve("agent:ada", invalid).is_err());

        assert_eq!(
            primary.conflicts().unwrap().len(),
            1,
            "conflict must remain resolvable"
        );
        assert_eq!(
            fs::read(primary.meta_root.join("merge-conflict").join(&path)).unwrap(),
            staged_file_before
        );
        assert_eq!(
            fs::read(&index_path).unwrap(),
            index_before,
            "all original index stages must be restored"
        );
        assert!(
            !marker.exists(),
            "a failed resolution must not leave a pending marker"
        );

        let mut valid = resolution;
        valid.content["instructions"] = json!("accepted");
        primary.resolve("agent:ada", valid).unwrap();
        assert!(primary.conflicts().unwrap().is_empty());
    }

    #[test]
    fn disjoint_edits_to_same_record_merge_as_a_real_three_way_git_merge() {
        let root = temp_root("three-way");
        let mut primary = approved_primary(root.join("primary"), "primary");
        let origin = primary.origin().to_owned();
        let mut laptop = FleetRepo::init_node(root.join("laptop"), "laptop", &origin).unwrap();
        let mut base = record("laptop");
        base.content =
            json!({"profile":{"name":"Ada"},"settings":{"tone":"clear","detail":"brief"}});
        let base = laptop.commit_record(base, None).unwrap();
        laptop.sync().unwrap();
        primary.sync().unwrap();
        let mut tablet = FleetRepo::init_node(root.join("tablet"), "tablet", &origin).unwrap();

        let mut left = base.clone();
        left.content["profile"]["name"] = json!("Augusta");
        laptop.commit_record(left, Some(&base.revision)).unwrap();
        let mut right = base.clone();
        right.content["settings"]["detail"] = json!("thorough");
        tablet.commit_record(right, Some(&base.revision)).unwrap();
        laptop.sync().unwrap();
        tablet.sync().unwrap();
        let outcome = primary.sync().unwrap();
        assert!(
            matches!(outcome, super::SyncOutcome::Merged { .. }),
            "{outcome:?}"
        );
        let merged = &primary.list_records().unwrap()[0];
        assert_eq!(merged.content["profile"]["name"], "Augusta");
        assert_eq!(merged.content["settings"]["detail"], "thorough");
        let parents = Command::new("git")
            .args([
                "-C",
                primary.repo_path().to_str().unwrap(),
                "rev-list",
                "--parents",
                "-n",
                "1",
                "HEAD",
            ])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&parents.stdout)
                .split_whitespace()
                .count(),
            3
        );
    }

    #[test]
    fn rejected_automatic_merge_keeps_original_conflict_stages() {
        let root = temp_root("rejected-auto-merge");
        let mut primary = approved_primary(root.join("primary"), "primary");
        let origin = primary.origin().to_owned();
        let mut laptop = FleetRepo::init_node(root.join("laptop"), "laptop", &origin).unwrap();
        let mut base = record("laptop");
        base.content = json!({"profile":{"name":"Ada"},"settings":{"detail":"brief"}});
        let base = laptop.commit_record(base, None).unwrap();
        laptop.sync().unwrap();
        primary.sync().unwrap();
        let mut tablet = FleetRepo::init_node(root.join("tablet"), "tablet", &origin).unwrap();
        let mut left = base.clone();
        left.content["profile"]["name"] = json!("Augusta");
        laptop.commit_record(left, Some(&base.revision)).unwrap();
        let mut right = base.clone();
        right.content["settings"]["detail"] = json!("thorough");
        tablet.commit_record(right, Some(&base.revision)).unwrap();
        laptop.sync().unwrap();
        tablet.sync().unwrap();

        primary.set_domain_validator(|records| {
            if records.iter().any(|record| {
                record.content["profile"]["name"] == "Augusta"
                    && record.content["settings"]["detail"] == "thorough"
            }) {
                Err("composed record needs an explicit review".into())
            } else {
                Ok(())
            }
        });
        let outcome = primary.sync().unwrap();
        assert!(
            matches!(outcome, super::SyncOutcome::Conflicted { .. }),
            "{outcome:?}"
        );
        let conflicts = primary.conflicts().unwrap();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(
            conflicts[0].base.as_ref().unwrap().content["settings"]["detail"],
            "brief"
        );
        assert_eq!(
            conflicts[0].current.as_ref().unwrap().content["profile"]["name"],
            "Augusta"
        );
        assert_eq!(
            conflicts[0].incoming.as_ref().unwrap().content["settings"]["detail"],
            "thorough"
        );
    }

    #[test]
    fn batch_resolution_combines_multiple_conflicts_with_absent_proof_records() {
        let root = temp_root("batch-resolution");
        let mut primary = approved_primary(root.join("primary"), "primary");
        let origin = primary.origin().to_owned();
        let mut node = FleetRepo::init_node(root.join("node"), "laptop", &origin).unwrap();
        let first = node.commit_record(record("laptop"), None).unwrap();
        let mut second = record("laptop");
        second.id = "agent:grace".into();
        let second = node.commit_record(second, None).unwrap();
        node.sync().unwrap();
        primary.sync().unwrap();

        for base in [&first, &second] {
            let mut local = base.clone();
            local.content["instructions"] = json!("primary edit");
            primary.commit_record(local, Some(&base.revision)).unwrap();
        }
        primary.sync().unwrap();
        for base in [&first, &second] {
            let mut incoming = base.clone();
            incoming.content["instructions"] = json!("node edit");
            node.commit_record(incoming, Some(&base.revision)).unwrap();
        }
        node.sync().unwrap();
        assert!(matches!(
            primary.sync().unwrap(),
            super::SyncOutcome::Conflicted { .. }
        ));
        let conflicts = primary.conflicts().unwrap();
        assert_eq!(conflicts.len(), 2);
        let resolutions = conflicts
            .iter()
            .map(|conflict| {
                let mut record = conflict.current.clone().unwrap();
                record.content["instructions"] = json!("reviewed");
                record
            })
            .collect::<Vec<_>>();
        let additions = (0..3)
            .map(|index| {
                let mut proof = record("primary");
                proof.id = format!("proof:{index}");
                proof.kind = RecordKind::Memory;
                proof.content = json!({"reviewed":true,"index":index});
                proof
            })
            .collect::<Vec<_>>();

        let resolved = primary.resolve_batch(resolutions, additions).unwrap();
        assert_eq!(resolved.len(), 5);
        assert!(primary.conflicts().unwrap().is_empty());
        let records = primary.list_records().unwrap();
        assert_eq!(records.len(), 5);
        assert_eq!(
            records
                .iter()
                .filter(|record| record.kind == RecordKind::Memory)
                .count(),
            3
        );
        assert!(
            records
                .iter()
                .filter(|record| record.kind == RecordKind::Agent)
                .all(|record| record.content["instructions"] == "reviewed")
        );
    }

    #[test]
    fn tombstone_conflicts_with_a_concurrent_edit_and_resolution_keeps_deletion() {
        let root = temp_root("tombstone");
        let mut primary = approved_primary(root.join("primary"), "primary");
        let origin = primary.origin().to_owned();
        let mut laptop = FleetRepo::init_node(root.join("laptop"), "laptop", &origin).unwrap();
        let mut base = record("laptop");
        base.content = json!({"instructions":"original"});
        let base = laptop.commit_record(base, None).unwrap();
        laptop.sync().unwrap();
        primary.sync().unwrap();
        let mut tablet = FleetRepo::init_node(root.join("tablet"), "tablet", &origin).unwrap();

        let mut deleted = base.clone();
        deleted.deleted = true;
        laptop.commit_record(deleted, Some(&base.revision)).unwrap();
        let mut changed = base.clone();
        changed.content["instructions"] = json!("new content");
        tablet.commit_record(changed, Some(&base.revision)).unwrap();
        laptop.sync().unwrap();
        tablet.sync().unwrap();
        let outcome = primary.sync().unwrap();
        assert!(
            matches!(outcome, super::SyncOutcome::Conflicted { .. }),
            "{outcome:?}"
        );
        let conflict = primary.conflicts().unwrap().pop().unwrap();
        let current = conflict.current.unwrap();
        assert!(current.deleted);
        assert!(!conflict.incoming.unwrap().deleted);
        let tombstone = current;
        primary.resolve("agent:ada", tombstone).unwrap();
        assert!(primary.list_records().unwrap()[0].deleted);
    }

    #[test]
    fn invalid_incoming_schema_never_advances_primary_main() {
        let root = temp_root("invalid");
        let mut primary = approved_primary(root.join("primary"), "primary");
        let origin = primary.origin().to_owned();
        let node = FleetRepo::init_node(root.join("node"), "laptop", &origin).unwrap();
        let mut invalid = record("laptop");
        invalid.schema_version = 999;
        let path = node.repo_path().join(super::record_path(&invalid));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        Command::new("git")
            .args([
                "-C",
                node.repo_path().to_str().unwrap(),
                "add",
                "--",
                super::record_path(&invalid).as_str(),
            ])
            .status()
            .unwrap();
        Command::new("git")
            .args([
                "-C",
                node.repo_path().to_str().unwrap(),
                "commit",
                "-m",
                "malformed incoming record",
            ])
            .status()
            .unwrap();
        Command::new("git")
            .args([
                "-C",
                node.repo_path().to_str().unwrap(),
                "push",
                "origin",
                "HEAD:refs/heads/nodes/laptop",
            ])
            .status()
            .unwrap();

        let before = primary.head().unwrap();
        assert!(primary.sync().is_err());
        assert_eq!(primary.head().unwrap(), before);
        assert!(primary.list_records().unwrap().is_empty());
    }

    #[test]
    fn duplicate_record_identity_in_incoming_tree_never_advances_main() {
        let root = temp_root("duplicate");
        let mut primary = approved_primary(root.join("primary"), "primary");
        let origin = primary.origin().to_owned();
        let node = FleetRepo::init_node(root.join("node"), "laptop", &origin).unwrap();
        let mut first = record("laptop");
        first.revision = super::record_digest(&first).unwrap();
        let first_path = node.repo_path().join(super::record_path(&first));
        fs::create_dir_all(first_path.parent().unwrap()).unwrap();
        fs::write(&first_path, serde_json::to_vec_pretty(&first).unwrap()).unwrap();
        let mut duplicate = first.clone();
        duplicate.kind = RecordKind::Skill;
        duplicate.revision = super::record_digest(&duplicate).unwrap();
        let duplicate_path = node.repo_path().join(super::record_path(&duplicate));
        fs::create_dir_all(duplicate_path.parent().unwrap()).unwrap();
        fs::write(
            &duplicate_path,
            serde_json::to_vec_pretty(&duplicate).unwrap(),
        )
        .unwrap();
        assert!(
            Command::new("git")
                .args(["-C", node.repo_path().to_str().unwrap(), "add", "records"])
                .output()
                .unwrap()
                .status
                .success()
        );
        assert!(
            Command::new("git")
                .args([
                    "-C",
                    node.repo_path().to_str().unwrap(),
                    "commit",
                    "-m",
                    "duplicate identity"
                ])
                .output()
                .unwrap()
                .status
                .success()
        );
        assert!(
            Command::new("git")
                .args([
                    "-C",
                    node.repo_path().to_str().unwrap(),
                    "push",
                    "origin",
                    "HEAD:refs/heads/nodes/laptop"
                ])
                .output()
                .unwrap()
                .status
                .success()
        );

        let before = primary.head().unwrap();
        assert!(primary.sync().is_err());
        assert_eq!(primary.head().unwrap(), before);
        assert!(primary.list_records().unwrap().is_empty());
    }

    #[test]
    fn uncommitted_record_files_never_appear_in_authoritative_listing() {
        let root = temp_root("uncommitted");
        let repo = approved_primary(root.join("primary"), "primary");
        let phantom = record("primary");
        let path = repo.repo_path().join(super::record_path(&phantom));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut phantom = phantom;
        phantom.revision = super::record_digest(&phantom).unwrap();
        fs::write(&path, serde_json::to_vec_pretty(&phantom).unwrap()).unwrap();
        assert!(repo.list_records().unwrap().is_empty());
        Command::new("git")
            .args([
                "-C",
                repo.repo_path().to_str().unwrap(),
                "add",
                "--",
                path.to_str().unwrap(),
            ])
            .status()
            .unwrap();
        assert!(repo.list_records().unwrap().is_empty());
        let primary_path = repo.data_root().to_path_buf();
        drop(repo);
        let reopened = FleetRepo::open_primary(&primary_path, "primary").unwrap();
        assert!(reopened.list_records().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn tracked_record_symlinks_are_rejected_without_following_them() {
        use std::os::unix::fs::symlink;

        let root = temp_root("record-symlink");
        let repo = approved_primary(root.join("primary"), "primary");
        let outside = root.join("outside.json");
        let mut target = record("primary");
        target.revision = super::record_digest(&target).unwrap();
        fs::write(&outside, serde_json::to_vec_pretty(&target).unwrap()).unwrap();
        let path = repo.repo_path().join(super::record_path(&target));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        symlink(&outside, &path).unwrap();
        Command::new("git")
            .args([
                "-C",
                repo.repo_path().to_str().unwrap(),
                "add",
                "--",
                path.to_str().unwrap(),
            ])
            .status()
            .unwrap();
        Command::new("git")
            .args([
                "-C",
                repo.repo_path().to_str().unwrap(),
                "commit",
                "-m",
                "tracked symlink",
            ])
            .status()
            .unwrap();
        assert!(repo.list_records().is_err());
    }

    #[test]
    fn record_identity_is_origin_qualified_and_cannot_be_reassigned() {
        let root = temp_root("origin-identity");
        let mut repo = approved_primary(root.join("primary"), "primary");
        let first = repo.commit_record(record("node-a"), None).unwrap();
        let mut second_origin = record("node-b");
        let second = repo.commit_record(second_origin.clone(), None).unwrap();
        assert_eq!(repo.list_records().unwrap().len(), 2);
        assert_ne!(first.revision, second.revision);

        let mut reassigned = first.clone();
        reassigned.origin_node_id = "node-b".into();
        reassigned.content["instructions"] = json!("Changed");
        assert!(
            repo.commit_record(reassigned, Some(&first.revision))
                .is_err()
        );
        second_origin.content["instructions"] = json!("Independent origin");
        assert!(
            repo.commit_record(second_origin, Some(&second.revision))
                .is_ok()
        );
    }

    #[test]
    fn primary_requires_a_domain_review_gate_before_main_changes() {
        let root = temp_root("domain-gate");
        let primary_path = root.join("primary");
        let mut primary = FleetRepo::init_primary(&primary_path, "primary").unwrap();
        let original_main = primary.head().unwrap();
        assert!(primary.commit_record(record("primary"), None).is_err());
        assert_eq!(primary.head().unwrap(), original_main);
        let origin = primary.origin().to_owned();
        let mut node = FleetRepo::init_node(root.join("node"), "laptop", &origin).unwrap();
        let mut approved = record("laptop");
        approved.content = json!({
            "instructions":"approved",
            "reviewHash":super::hex(&Sha256::digest(b"approved"))
        });
        let base = node.commit_record(approved, None).unwrap();
        node.sync().unwrap();
        let before = primary.head().unwrap();
        assert!(primary.sync().is_err());
        assert_eq!(primary.head().unwrap(), before);

        primary.set_domain_validator(|records| {
            for record in records
                .iter()
                .filter(|record| record.kind == RecordKind::Agent)
            {
                let instructions = record.content["instructions"]
                    .as_str()
                    .ok_or_else(|| "agent instructions are missing".to_owned())?;
                let expected = super::hex(&Sha256::digest(instructions.as_bytes()));
                if record.content["reviewHash"] != expected {
                    return Err("agent instructions need a matching review".into());
                }
            }
            Ok(())
        });
        let mut edited = base.clone();
        edited.content["instructions"] = json!("unreviewed instructions");
        node.commit_record(edited.clone(), Some(&base.revision))
            .unwrap();
        node.sync().unwrap();
        let before = primary.head().unwrap();
        assert!(
            primary
                .sync()
                .unwrap_err()
                .to_string()
                .contains("domain validation")
        );
        assert_eq!(primary.head().unwrap(), before);
        assert!(primary.list_records().unwrap().is_empty());

        edited.content["reviewHash"] = json!(super::hex(&Sha256::digest(
            edited.content["instructions"].as_str().unwrap().as_bytes()
        )));
        node.commit_record(edited, Some(&node.list_records().unwrap()[0].revision))
            .unwrap();
        node.sync().unwrap();
        assert!(matches!(
            primary.sync().unwrap(),
            super::SyncOutcome::Merged { .. }
        ));
    }

    #[test]
    fn execution_projection_changes_must_come_from_the_owning_node_branch() {
        let root = temp_root("projection-owner");
        let mut primary = approved_primary(root.join("primary"), "primary");
        let origin = primary.origin().to_owned();
        let mut node = FleetRepo::init_node(root.join("node"), "laptop", &origin).unwrap();
        let mut transcript = record("other-node");
        transcript.kind = RecordKind::Transcript;
        transcript.id = "transcript:one".into();
        node.commit_record(transcript, None).unwrap();
        node.sync().unwrap();
        let before = primary.head().unwrap();
        let error = primary.sync().unwrap_err().to_string();
        assert!(error.contains("cannot publish execution projection"));
        assert_eq!(primary.head().unwrap(), before);
        assert!(primary.list_records().unwrap().is_empty());
    }

    #[test]
    fn node_conflicts_stay_staged_and_do_not_block_local_edits_or_later_recovery() {
        let root = temp_root("node-recovery");
        let mut primary = approved_primary(root.join("primary"), "primary");
        let origin = primary.origin().to_owned();
        let mut node = FleetRepo::init_node(root.join("node"), "laptop", &origin).unwrap();
        let base = node.commit_record(record("laptop"), None).unwrap();
        node.sync().unwrap();
        primary.sync().unwrap();

        let mut primary_edit = base.clone();
        primary_edit.content["instructions"] = json!("Primary change");
        primary
            .commit_record(primary_edit, Some(&base.revision))
            .unwrap();
        primary.sync().unwrap();
        let mut node_edit = base.clone();
        node_edit.content["instructions"] = json!("Node change");
        node.commit_record(node_edit, Some(&base.revision)).unwrap();
        assert!(matches!(
            node.sync().unwrap(),
            super::SyncOutcome::Conflicted { .. }
        ));

        drop(node);
        let mut node = FleetRepo::open_node(root.join("node"), "laptop", &origin).unwrap();
        assert_eq!(node.conflicts().unwrap().len(), 1);
        assert!(
            node.list_records()
                .unwrap()
                .iter()
                .any(|r| r.content["instructions"] == "Node change")
        );

        let local_only = Record {
            id: "agent:grace".into(),
            kind: RecordKind::Agent,
            origin_node_id: "laptop".into(),
            revision: String::new(),
            base_revision: None,
            deleted: false,
            content: json!({"name":"Grace","instructions":"Offline edit"}),
            schema_version: 1,
        };
        assert!(node.commit_record(local_only, None).is_ok());

        let primary_conflict = primary.sync().unwrap();
        assert!(matches!(
            primary_conflict,
            super::SyncOutcome::Conflicted { .. }
        ));
        let conflict = primary.conflicts().unwrap().pop().unwrap();
        let mut resolution = conflict.current.unwrap();
        resolution.content["instructions"] = json!("Primary resolution");
        primary.resolve("agent:ada", resolution).unwrap();

        assert!(matches!(
            node.sync().unwrap(),
            super::SyncOutcome::Published { .. } | super::SyncOutcome::UpToDate
        ));
        assert!(node.conflicts().unwrap().is_empty());
        let records = node.list_records().unwrap();
        assert!(
            records
                .iter()
                .any(|r| r.content["instructions"] == "Primary resolution")
        );
        assert!(records.iter().any(|r| r.id == "agent:grace"));
    }

    #[test]
    fn reopen_finishes_a_staged_conflict_resolution_after_crash() {
        let root = temp_root("resolution-recovery");
        let primary_path = root.join("primary");
        let mut primary = approved_primary(&primary_path, "primary");
        let origin = primary.origin().to_owned();
        let mut node = FleetRepo::init_node(root.join("node"), "laptop", &origin).unwrap();
        let base = node.commit_record(record("laptop"), None).unwrap();
        node.sync().unwrap();
        primary.sync().unwrap();

        let mut primary_edit = base.clone();
        primary_edit.content["instructions"] = json!("Primary edit");
        primary
            .commit_record(primary_edit, Some(&base.revision))
            .unwrap();
        primary.sync().unwrap();
        let mut node_edit = base.clone();
        node_edit.content["instructions"] = json!("Node edit");
        node.commit_record(node_edit, Some(&base.revision)).unwrap();
        node.sync().unwrap();
        assert!(matches!(
            primary.sync().unwrap(),
            super::SyncOutcome::Conflicted { .. }
        ));

        let conflict = primary.conflicts().unwrap().pop().unwrap();
        let mut resolution = conflict.current.unwrap();
        resolution.content["instructions"] = json!("Recovered resolution");
        resolution.revision = super::record_digest(&resolution).unwrap();
        let stage = primary.meta_root.join("merge-conflict");
        let record_path = super::record_path(&resolution);
        fs::write(
            stage.join(&record_path),
            serde_json::to_vec_pretty(&resolution).unwrap(),
        )
        .unwrap();
        fs::write(primary.meta_root.join("resolution.pending"), "primary").unwrap();
        Command::new("git")
            .args([
                "-C",
                stage.to_str().unwrap(),
                "add",
                "--",
                record_path.as_str(),
            ])
            .status()
            .unwrap();
        drop(primary);

        let mut reopened = FleetRepo::open_primary(&primary_path, "primary").unwrap();
        reopened.set_domain_validator(|_| Ok(()));
        assert!(matches!(
            reopened.sync().unwrap(),
            super::SyncOutcome::Merged { .. }
        ));
        assert!(reopened.conflicts().unwrap().is_empty());
        assert_eq!(
            reopened.list_records().unwrap()[0].content["instructions"],
            "Recovered resolution"
        );
        assert!(!reopened.meta_root.join("resolution.pending").exists());
    }
}
