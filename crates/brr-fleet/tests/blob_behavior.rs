use brr_fleet::{BlobFile, FleetRepo, Record, RecordKind};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf, time::SystemTime};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn temp_root() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "brr-fleet-blob-behavior-{}-{nonce}",
        std::process::id()
    ))
}

#[test]
fn blob_payload_is_stored_as_raw_git_file_and_record_stays_metadata_only() {
    let root = temp_root();
    let bytes = b"hello";
    let sha = hex(&Sha256::digest(bytes));
    fs::create_dir_all(&root).unwrap();
    let source = root.join("source.bin");
    fs::write(&source, bytes).unwrap();
    let source = fs::canonicalize(source).unwrap();
    let record = Record {
        schema_version: 1,
        id: sha.clone(),
        kind: RecordKind::Blob,
        origin_node_id: "primary".into(),
        revision: String::new(),
        base_revision: None,
        deleted: false,
        content: json!({"sha256":sha,"sizeBytes":bytes.len()}),
    };

    let result = (|| {
        let mut repo = FleetRepo::init_primary(&root, "primary")?;
        repo.set_domain_validator(|_| Ok(()));
        repo.commit_records_with_blobs(
            vec![(record, None)],
            vec![BlobFile {
                sha256: sha.clone(),
                size_bytes: bytes.len() as u64,
                source_path: source.clone(),
            }],
        )?;
        repo.sync()?;
        let saved = repo.list_records()?;
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].content, json!({"sha256":sha,"sizeBytes":5}));
        assert_eq!(fs::read(repo.repo_path().join("blobs").join(&sha))?, bytes);
        let clone_root = root.join("worker");
        let mut clone = FleetRepo::init_node(&clone_root, "worker", repo.origin())?;
        clone.sync()?;
        assert_eq!(clone.list_records()?, saved);
        assert_eq!(fs::read(clone.repo_path().join("blobs").join(&sha))?, bytes);
        Ok::<_, Box<dyn std::error::Error>>(())
    })();
    let _ = fs::remove_dir_all(&root);
    result.unwrap();
}

#[test]
fn blob_source_is_rehashed_and_failure_leaves_no_record_or_raw_file() {
    let root = temp_root();
    fs::create_dir_all(&root).unwrap();
    let bytes = b"accepted bytes";
    let sha = hex(&Sha256::digest(bytes));
    let source = root.join("source.bin");
    fs::write(&source, bytes).unwrap();
    let source = fs::canonicalize(source).unwrap();
    let record = Record {
        schema_version: 1,
        id: sha.clone(),
        kind: RecordKind::Blob,
        origin_node_id: "primary".into(),
        revision: String::new(),
        base_revision: None,
        deleted: false,
        content: json!({"sha256":sha,"sizeBytes":bytes.len()}),
    };
    let mut repo = FleetRepo::init_primary(&root, "primary").unwrap();
    repo.set_domain_validator(|_| Ok(()));
    let before = repo.head().unwrap();
    fs::write(&source, b"tampered bytes").unwrap();
    let error = repo
        .commit_records_with_blobs(
            vec![(record, None)],
            vec![BlobFile {
                sha256: sha.clone(),
                size_bytes: bytes.len() as u64,
                source_path: source,
            }],
        )
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("blob source size or SHA-256 mismatch"),
        "{error}"
    );
    assert_eq!(repo.head().unwrap(), before);
    assert!(repo.list_records().unwrap().is_empty());
    assert!(!repo.repo_path().join("blobs").join(sha).exists());
    let _ = fs::remove_dir_all(root);
}
