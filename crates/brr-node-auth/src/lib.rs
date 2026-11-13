//! Pairing and TLS identity for nodes in an owner-managed personal fleet.
//!
//! The caller is responsible for exposing invite creation through a local,
//! owner-authorized control channel. This crate never opens a network listener.

use getrandom::fill as random_fill;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose, SanType,
};
use rustls::{
    pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer},
    ServerConfig,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    Row, SqlitePool,
};
use std::{
    net::IpAddr,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceRole {
    Mobile,
    Node,
}

impl DeviceRole {
    fn as_db(self) -> &'static str {
        match self {
            Self::Mobile => "mobile",
            Self::Node => "node",
        }
    }
    fn parse(value: &str) -> Result<Self, AuthError> {
        match value {
            "mobile" => Ok(Self::Mobile),
            "node" => Ok(Self::Node),
            _ => Err(AuthError::Storage),
        }
    }
}

#[derive(Debug)]
pub enum AuthError {
    Storage,
    Io(std::io::Error),
    UnsafeDatabasePath,
    UnprivateDatabaseParent,
    InvalidEndpoints,
    InvalidExpiry,
    InvalidInvite,
    InviteExpired,
    InviteAlreadyUsed,
    WrongRole,
    InvalidCredential,
    DeviceNotFound,
    Certificate,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Storage => "authentication storage operation failed",
            Self::Io(_) => "authentication storage file operation failed",
            Self::UnsafeDatabasePath => {
                "authentication database path must be a regular non-symlink file"
            }
            Self::UnprivateDatabaseParent => {
                "authentication database parent must be owned and private"
            }
            Self::InvalidEndpoints => "at least one valid DNS or IP endpoint is required",
            Self::InvalidExpiry => "invite expiry must be greater than zero",
            Self::InvalidInvite => "invite is invalid",
            Self::InviteExpired => "invite has expired",
            Self::InviteAlreadyUsed => "invite has already been used",
            Self::WrongRole => "invite role does not match the requested device role",
            Self::InvalidCredential => "device credential is invalid or revoked",
            Self::DeviceNotFound => "device was not found",
            Self::Certificate => "TLS identity could not be created or loaded",
        })
    }
}
impl std::error::Error for AuthError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}
impl From<std::io::Error> for AuthError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
impl From<sqlx::Error> for AuthError {
    fn from(_: sqlx::Error) -> Self {
        Self::Storage
    }
}

#[derive(Clone)]
pub struct NodeAuthStore {
    pool: SqlitePool,
    identity: std::sync::Arc<Identity>,
}

struct Identity {
    node_id: String,
    endpoints: Vec<String>,
    ca_certificate_pem: String,
    ca_private_key_der: Vec<u8>,
    certificate_pem: String,
    private_key_der: Vec<u8>,
    fingerprint: String,
}

/// Public identity information safe to display or put into a QR payload.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NodeIdentityInfo {
    pub node_id: String,
    pub certificate_pem: String,
    pub fingerprint: String,
    pub endpoints: Vec<String>,
}

/// An invite contains a bearer code. Avoid logging or persisting this value.
pub struct PairingInvite {
    pub code: String,
    pub expires_at_unix: i64,
    pub role: DeviceRole,
    pub node_id: String,
    pub server_certificate_pem: String,
    pub fingerprint: String,
}
impl std::fmt::Debug for PairingInvite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingInvite")
            .field("code", &"[REDACTED]")
            .field("expires_at_unix", &self.expires_at_unix)
            .field("role", &self.role)
            .field("node_id", &self.node_id)
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}

/// A newly paired device's bearer credential. Store it in the native credential store.
pub struct DeviceCredential {
    pub device_id: String,
    pub name: String,
    pub role: DeviceRole,
    pub node_id: String,
    pub secret: String,
    pub trusted_server_certificate_pem: String,
    pub fingerprint: String,
}
impl std::fmt::Debug for DeviceCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceCredential")
            .field("device_id", &self.device_id)
            .field("name", &self.name)
            .field("role", &self.role)
            .field("node_id", &self.node_id)
            .field("secret", &"[REDACTED]")
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Principal {
    pub id: String,
    pub name: String,
    pub role: DeviceRole,
    #[serde(rename = "nodeId")]
    pub node_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub role: DeviceRole,
    pub node_id: String,
    pub created_at_unix: i64,
}

impl NodeAuthStore {
    /// Open the SQLite database at `db_path`. The first call creates a stable
    /// node identity whose certificate SANs include every configured DNS/IP endpoint.
    pub async fn open(
        db_path: impl AsRef<Path>,
        endpoints: Vec<String>,
    ) -> Result<Self, AuthError> {
        let endpoints = normalize_endpoints(endpoints)?;
        let path: PathBuf = db_path.as_ref().to_path_buf();
        if !path.is_absolute() {
            return Err(AuthError::UnsafeDatabasePath);
        }
        let parent = path.parent().ok_or(AuthError::UnsafeDatabasePath)?;
        ensure_private_parent(parent)?;
        create_private_file(&path)?;
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        sqlx::query("CREATE TABLE IF NOT EXISTS node_identity (singleton INTEGER PRIMARY KEY CHECK(singleton=1), node_id TEXT NOT NULL, endpoints TEXT NOT NULL, ca_cert_pem TEXT NOT NULL, ca_key_der BLOB NOT NULL, cert_pem TEXT NOT NULL, key_der BLOB NOT NULL, fingerprint TEXT NOT NULL)").execute(&pool).await?;
        sqlx::query("CREATE TABLE IF NOT EXISTS pairing_invites (code_hash BLOB PRIMARY KEY, role TEXT NOT NULL, expires_at INTEGER NOT NULL, used_at INTEGER)").execute(&pool).await?;
        sqlx::query("CREATE TABLE IF NOT EXISTS devices (id TEXT PRIMARY KEY, name TEXT NOT NULL, role TEXT NOT NULL, node_id TEXT NOT NULL, secret_hash BLOB NOT NULL UNIQUE, created_at INTEGER NOT NULL, revoked_at INTEGER)").execute(&pool).await?;

        let identity = match sqlx::query("SELECT node_id, endpoints, ca_cert_pem, ca_key_der, cert_pem, key_der, fingerprint FROM node_identity WHERE singleton=1").fetch_optional(&pool).await? {
            Some(row) => {
                let persisted: Vec<String> = serde_json::from_str(row.try_get::<&str, _>("endpoints")?).map_err(|_| AuthError::Storage)?;
                let mut identity = Identity { node_id: row.try_get("node_id")?, endpoints: persisted,
                    ca_certificate_pem: row.try_get("ca_cert_pem")?, ca_private_key_der: row.try_get("ca_key_der")?,
                    certificate_pem: row.try_get("cert_pem")?, private_key_der: row.try_get("key_der")?, fingerprint: row.try_get("fingerprint")? };
                if identity.endpoints != endpoints {
                    let (leaf_pem, leaf_key) = issue_leaf(&endpoints, &identity.ca_private_key_der)?;
                    sqlx::query("UPDATE node_identity SET endpoints=?,cert_pem=?,key_der=? WHERE singleton=1")
                        .bind(serde_json::to_string(&endpoints).map_err(|_| AuthError::Storage)?)
                        .bind(&leaf_pem).bind(&leaf_key).execute(&pool).await?;
                    identity.endpoints = endpoints;
                    identity.certificate_pem = leaf_pem;
                    identity.private_key_der = leaf_key;
                }
                identity
            }
            None => {
                let identity = create_identity(endpoints)?;
                sqlx::query("INSERT INTO node_identity(singleton,node_id,endpoints,ca_cert_pem,ca_key_der,cert_pem,key_der,fingerprint) VALUES(1,?,?,?,?,?,?,?)")
                    .bind(&identity.node_id).bind(serde_json::to_string(&identity.endpoints).map_err(|_| AuthError::Storage)?)
                    .bind(&identity.ca_certificate_pem).bind(&identity.ca_private_key_der)
                    .bind(&identity.certificate_pem).bind(&identity.private_key_der).bind(&identity.fingerprint).execute(&pool).await?;
                identity
            }
        };
        Ok(Self {
            pool,
            identity: std::sync::Arc::new(identity),
        })
    }

    pub fn node_id(&self) -> &str {
        &self.identity.node_id
    }
    /// CA certificate trusted by paired clients. The serving leaf is kept internal.
    pub fn certificate_pem(&self) -> &str {
        &self.identity.ca_certificate_pem
    }
    pub fn trusted_server_cert_pem(&self) -> &str {
        &self.identity.ca_certificate_pem
    }
    pub fn fingerprint(&self) -> &str {
        &self.identity.fingerprint
    }
    pub fn identity_info(&self) -> NodeIdentityInfo {
        NodeIdentityInfo {
            node_id: self.identity.node_id.clone(),
            certificate_pem: self.identity.ca_certificate_pem.clone(),
            fingerprint: self.identity.fingerprint.clone(),
            endpoints: self.identity.endpoints.clone(),
        }
    }

    /// Configure a TLS server using this node's self-signed certificate.
    pub fn tls_server_config(&self) -> Result<ServerConfig, AuthError> {
        let leaf_pem = self.identity.certificate_pem.as_bytes();
        let mut leaf_reader = &leaf_pem[..];
        let mut certs = rustls_pemfile::certs(&mut leaf_reader);
        let leaf = certs
            .next()
            .ok_or(AuthError::Certificate)?
            .map_err(|_| AuthError::Certificate)?;
        let ca_pem = self.identity.ca_certificate_pem.as_bytes();
        let ca = rustls_pemfile::certs(&mut &ca_pem[..])
            .next()
            .ok_or(AuthError::Certificate)?
            .map_err(|_| AuthError::Certificate)?;
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            self.identity.private_key_der.clone(),
        ));
        ServerConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| AuthError::Certificate)?
        .with_no_client_auth()
        .with_single_cert(vec![leaf, ca], key)
        .map_err(|_| AuthError::Certificate)
    }

    /// Build a normal hostname-verifying client config trusting only the supplied
    /// paired CA. Callers must still pass the advertised DNS/IP name to rustls.
    pub fn tls_client_config(trusted_ca_pem: &str) -> Result<rustls::ClientConfig, AuthError> {
        let mut roots = rustls::RootCertStore::empty();
        let trusted_ca_bytes = trusted_ca_pem.as_bytes();
        let mut ca_reader = &trusted_ca_bytes[..];
        let mut certs = rustls_pemfile::certs(&mut ca_reader);
        let ca = certs
            .next()
            .ok_or(AuthError::Certificate)?
            .map_err(|_| AuthError::Certificate)?;
        roots.add(ca).map_err(|_| AuthError::Certificate)?;
        Ok(
            rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
                rustls::crypto::aws_lc_rs::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .map_err(|_| AuthError::Certificate)?
            .with_root_certificates(roots)
            .with_no_client_auth(),
        )
    }

    /// Create a role-bound, 256-bit, expiring, single-use pairing invite.
    pub async fn create_invite(
        &self,
        role: DeviceRole,
        expires_in: Duration,
    ) -> Result<PairingInvite, AuthError> {
        if expires_in.is_zero() {
            return Err(AuthError::InvalidExpiry);
        }
        let expires_at = now_unix()?
            .checked_add(i64::try_from(expires_in.as_secs()).map_err(|_| AuthError::InvalidExpiry)?)
            .ok_or(AuthError::InvalidExpiry)?;
        let code = random_token()?;
        sqlx::query(
            "INSERT INTO pairing_invites(code_hash,role,expires_at,used_at) VALUES(?,?,?,NULL)",
        )
        .bind(digest(code.as_bytes()).to_vec())
        .bind(role.as_db())
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        Ok(PairingInvite {
            code,
            expires_at_unix: expires_at,
            role,
            node_id: self.identity.node_id.clone(),
            server_certificate_pem: self.identity.ca_certificate_pem.clone(),
            fingerprint: self.identity.fingerprint.clone(),
        })
    }

    /// Redeem an invite exactly once. `requested_role` is checked against its
    /// stored role before any device credential is issued.
    pub async fn redeem_invite(
        &self,
        code: &str,
        requested_role: DeviceRole,
        name: &str,
    ) -> Result<DeviceCredential, AuthError> {
        if name.trim().is_empty() {
            return Err(AuthError::InvalidCredential);
        }
        let code_hash = digest(code.as_bytes()).to_vec();
        let mut tx = self.pool.begin().await?;
        let row =
            sqlx::query("SELECT role, expires_at, used_at FROM pairing_invites WHERE code_hash=?")
                .bind(code_hash.clone())
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(AuthError::InvalidInvite)?;
        if row.try_get::<Option<i64>, _>("used_at")?.is_some() {
            return Err(AuthError::InviteAlreadyUsed);
        }
        let role = DeviceRole::parse(row.try_get("role")?)?;
        if role != requested_role {
            return Err(AuthError::WrongRole);
        }
        if row.try_get::<i64, _>("expires_at")? <= now_unix()? {
            return Err(AuthError::InviteExpired);
        }
        let secret = random_token()?;
        let device_id = random_id("device")?;
        let created_at = now_unix()?;
        let updated = sqlx::query("UPDATE pairing_invites SET used_at=? WHERE code_hash=? AND used_at IS NULL AND expires_at>? ")
            .bind(created_at).bind(code_hash).bind(created_at).execute(&mut *tx).await?.rows_affected();
        if updated != 1 {
            return Err(AuthError::InviteAlreadyUsed);
        }
        sqlx::query("INSERT INTO devices(id,name,role,node_id,secret_hash,created_at,revoked_at) VALUES(?,?,?,?,?,?,NULL)")
            .bind(&device_id).bind(name.trim()).bind(role.as_db()).bind(&self.identity.node_id).bind(digest(secret.as_bytes()).to_vec()).bind(created_at).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(DeviceCredential {
            device_id,
            name: name.trim().to_owned(),
            role,
            node_id: self.identity.node_id.clone(),
            secret,
            trusted_server_certificate_pem: self.identity.ca_certificate_pem.clone(),
            fingerprint: self.identity.fingerprint.clone(),
        })
    }

    /// Authenticate an opaque device bearer and return its typed principal.
    pub async fn authenticate(&self, secret: &str) -> Result<Principal, AuthError> {
        let row = sqlx::query(
            "SELECT id,name,role,node_id FROM devices WHERE secret_hash=? AND revoked_at IS NULL",
        )
        .bind(digest(secret.as_bytes()).to_vec())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(AuthError::InvalidCredential)?;
        Ok(Principal {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            role: DeviceRole::parse(row.try_get("role")?)?,
            node_id: row.try_get("node_id")?,
        })
    }

    pub async fn revoke(&self, device_id: &str) -> Result<(), AuthError> {
        let changed =
            sqlx::query("UPDATE devices SET revoked_at=? WHERE id=? AND revoked_at IS NULL")
                .bind(now_unix()?)
                .bind(device_id)
                .execute(&self.pool)
                .await?
                .rows_affected();
        if changed == 0 {
            return Err(AuthError::DeviceNotFound);
        }
        Ok(())
    }

    pub async fn list_devices(&self) -> Result<Vec<DeviceInfo>, AuthError> {
        let rows = sqlx::query("SELECT id,name,role,node_id,created_at FROM devices WHERE revoked_at IS NULL ORDER BY created_at,id").fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                Ok(DeviceInfo {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    role: DeviceRole::parse(row.try_get("role")?)?,
                    node_id: row.try_get("node_id")?,
                    created_at_unix: row.try_get("created_at")?,
                })
            })
            .collect()
    }
}

fn normalize_endpoints(endpoints: Vec<String>) -> Result<Vec<String>, AuthError> {
    let mut clean = Vec::new();
    for endpoint in endpoints {
        let endpoint = endpoint.trim().trim_end_matches('.').to_ascii_lowercase();
        if endpoint.is_empty()
            || endpoint.contains('/')
            || endpoint.contains(':') && endpoint.parse::<IpAddr>().is_err()
        {
            return Err(AuthError::InvalidEndpoints);
        }
        if endpoint.parse::<IpAddr>().is_err()
            && (!endpoint.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    && !label.starts_with('-')
                    && !label.ends_with('-')
            }) || endpoint.len() > 253)
        {
            return Err(AuthError::InvalidEndpoints);
        }
        clean.push(endpoint);
    }
    clean.sort();
    clean.dedup();
    if clean.is_empty() {
        Err(AuthError::InvalidEndpoints)
    } else {
        Ok(clean)
    }
}

fn create_identity(endpoints: Vec<String>) -> Result<Identity, AuthError> {
    let ca_key = KeyPair::generate().map_err(|_| AuthError::Certificate)?;
    let ca_cert = ca_params()
        .self_signed(&ca_key)
        .map_err(|_| AuthError::Certificate)?;
    let ca_certificate_pem = ca_cert.pem();
    let ca_private_key_der = ca_key.serialize_der();
    let (certificate_pem, private_key_der) = issue_leaf(&endpoints, &ca_private_key_der)?;
    let fingerprint = fingerprint(ca_cert.der().as_ref());
    Ok(Identity {
        node_id: random_id("node")?,
        endpoints,
        ca_certificate_pem,
        ca_private_key_der,
        certificate_pem,
        private_key_der,
        fingerprint,
    })
}

fn ca_params() -> CertificateParams {
    let mut params = CertificateParams::default();
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, "Brr Fleet Node CA");
    params.distinguished_name = name;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params
}

fn issue_leaf(endpoints: &[String], ca_key_der: &[u8]) -> Result<(String, Vec<u8>), AuthError> {
    let mut params = CertificateParams::default();
    params.subject_alt_names = endpoints
        .iter()
        .map(|endpoint| {
            Ok(match endpoint.parse::<IpAddr>() {
                Ok(ip) => SanType::IpAddress(ip),
                Err(_) => SanType::DnsName(
                    endpoint
                        .clone()
                        .try_into()
                        .map_err(|_| AuthError::InvalidEndpoints)?,
                ),
            })
        })
        .collect::<Result<Vec<_>, AuthError>>()?;
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, "Brr Fleet Node");
    params.distinguished_name = name;
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let key = KeyPair::generate().map_err(|_| AuthError::Certificate)?;
    let ca_key = KeyPair::try_from(ca_key_der).map_err(|_| AuthError::Certificate)?;
    let issuer = Issuer::new(ca_params(), ca_key);
    let cert = params
        .signed_by(&key, &issuer)
        .map_err(|_| AuthError::Certificate)?;
    Ok((cert.pem(), key.serialize_der()))
}

fn ensure_private_parent(parent: &Path) -> Result<(), AuthError> {
    let metadata =
        std::fs::symlink_metadata(parent).map_err(|_| AuthError::UnprivateDatabaseParent)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AuthError::UnprivateDatabaseParent);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(AuthError::UnprivateDatabaseParent);
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(AuthError::UnprivateDatabaseParent);
        }
    }
    Ok(())
}

fn create_private_file(path: &Path) -> Result<(), AuthError> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(AuthError::UnsafeDatabasePath);
        }
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let file = options.open(path).map_err(|error| {
            if error.raw_os_error() == Some(libc::ELOOP) {
                AuthError::UnsafeDatabasePath
            } else {
                AuthError::Io(error)
            }
        })?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(AuthError::UnsafeDatabasePath);
        }
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o600);
        file.set_permissions(permissions)?;
    }
    #[cfg(not(unix))]
    {
        let file = options.open(path)?;
        if !file.metadata()?.is_file() {
            return Err(AuthError::UnsafeDatabasePath);
        }
    }
    Ok(())
}
fn random_token() -> Result<String, AuthError> {
    let mut bytes = [0u8; 32];
    random_fill(&mut bytes).map_err(|_| AuthError::Storage)?;
    Ok(hex(&bytes))
}
fn random_id(prefix: &str) -> Result<String, AuthError> {
    Ok(format!("{prefix}_{}", random_token()?))
}
fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
fn fingerprint(cert_der: &[u8]) -> String {
    format!("sha256:{}", hex(&digest(cert_der)))
}
fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}
fn now_unix() -> Result<i64, AuthError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AuthError::Storage)
        .and_then(|d| i64::try_from(d.as_secs()).map_err(|_| AuthError::Storage))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::{pki_types::ServerName, ClientConnection, ServerConnection};
    use std::{io::Cursor, sync::Arc};
    use tempfile::tempdir;

    fn private_tempdir() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        dir
    }

    async fn store() -> (tempfile::TempDir, NodeAuthStore) {
        let dir = private_tempdir();
        let store = NodeAuthStore::open(
            dir.path().join("auth.sqlite"),
            vec!["fleet.local".into(), "192.168.1.5".into()],
        )
        .await
        .unwrap();
        (dir, store)
    }

    #[tokio::test]
    async fn invite_is_single_use_and_credentials_authenticate() {
        let (_dir, store) = store().await;
        let invite = store
            .create_invite(DeviceRole::Mobile, Duration::from_secs(60))
            .await
            .unwrap();
        let credential = store
            .redeem_invite(&invite.code, DeviceRole::Mobile, "phone")
            .await
            .unwrap();
        assert_eq!(
            store.authenticate(&credential.secret).await.unwrap().role,
            DeviceRole::Mobile
        );
        assert!(matches!(
            store
                .redeem_invite(&invite.code, DeviceRole::Mobile, "again")
                .await,
            Err(AuthError::InviteAlreadyUsed)
        ));
    }

    #[tokio::test]
    async fn concurrent_invite_redemption_issues_at_most_one_credential() {
        let (_dir, store) = store().await;
        let invite = store
            .create_invite(DeviceRole::Node, Duration::from_secs(30))
            .await
            .unwrap();
        let (first, second) = tokio::join!(
            store.redeem_invite(&invite.code, DeviceRole::Node, "node-a"),
            store.redeem_invite(&invite.code, DeviceRole::Node, "node-b"),
        );
        assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    }

    #[tokio::test]
    async fn invite_expiry_and_wrong_role_are_rejected() {
        let (_dir, store) = store().await;
        assert!(matches!(
            store.create_invite(DeviceRole::Node, Duration::ZERO).await,
            Err(AuthError::InvalidExpiry)
        ));
        let invite = store
            .create_invite(DeviceRole::Node, Duration::from_secs(1))
            .await
            .unwrap();
        assert!(matches!(
            store
                .redeem_invite(&invite.code, DeviceRole::Mobile, "phone")
                .await,
            Err(AuthError::WrongRole)
        ));
        tokio::time::sleep(Duration::from_millis(1_050)).await;
        assert!(matches!(
            store
                .redeem_invite(&invite.code, DeviceRole::Node, "node")
                .await,
            Err(AuthError::InviteExpired)
        ));
    }

    #[tokio::test]
    async fn revoke_invalidates_credential_and_list_excludes_revoked() {
        let (_dir, store) = store().await;
        let invite = store
            .create_invite(DeviceRole::Node, Duration::from_secs(60))
            .await
            .unwrap();
        let credential = store
            .redeem_invite(&invite.code, DeviceRole::Node, "worker")
            .await
            .unwrap();
        store.revoke(&credential.device_id).await.unwrap();
        assert!(matches!(
            store.authenticate(&credential.secret).await,
            Err(AuthError::InvalidCredential)
        ));
        assert!(store.list_devices().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn identity_persists_ca_and_reissues_leaf_for_changed_endpoints() {
        let dir = private_tempdir();
        let db = dir.path().join("auth.sqlite");
        let first = NodeAuthStore::open(&db, vec!["fleet.local".into(), "10.0.0.4".into()])
            .await
            .unwrap();
        let node_id = first.node_id().to_owned();
        let fingerprint = first.fingerprint().to_owned();
        assert!(first.certificate_pem().contains("BEGIN CERTIFICATE"));
        let ca_pem = first.certificate_pem().to_owned();
        assert!(first.tls_server_config().is_ok());
        drop(first);
        let reopened = NodeAuthStore::open(&db, vec!["10.0.0.4".into(), "fleet.local".into()])
            .await
            .unwrap();
        assert_eq!(reopened.node_id(), node_id);
        assert_eq!(reopened.fingerprint(), fingerprint);
        assert_eq!(reopened.certificate_pem(), ca_pem);
        let reconfigured = NodeAuthStore::open(&db, vec!["new.local".into()])
            .await
            .unwrap();
        assert_eq!(reconfigured.node_id(), node_id);
        assert_eq!(reconfigured.fingerprint(), fingerprint);
        assert_eq!(reconfigured.certificate_pem(), ca_pem);
        assert!(reconfigured.tls_server_config().is_ok());
        assert!(NodeAuthStore::tls_client_config(reconfigured.certificate_pem()).is_ok());
    }

    #[tokio::test]
    async fn rustls_handshake_trusts_paired_ca_and_checks_dns_ip_names() {
        let (dir, store) = store().await;
        let server_config = store.tls_server_config().unwrap();
        let client_config = NodeAuthStore::tls_client_config(store.certificate_pem()).unwrap();
        assert!(handshake(client_config.clone(), server_config.clone(), "fleet.local").is_ok());
        assert!(handshake(client_config.clone(), server_config.clone(), "192.168.1.5").is_ok());
        assert!(handshake(client_config, server_config, "attacker.local").is_err());

        let other =
            NodeAuthStore::open(dir.path().join("other.sqlite"), vec!["other.local".into()])
                .await
                .unwrap();
        let unpaired_client = NodeAuthStore::tls_client_config(other.certificate_pem()).unwrap();
        assert!(handshake(
            unpaired_client,
            store.tls_server_config().unwrap(),
            "fleet.local"
        )
        .is_err());
    }

    #[tokio::test]
    async fn credential_and_invite_codes_are_not_stored_in_plaintext() {
        let (dir, store) = store().await;
        let invite = store
            .create_invite(DeviceRole::Mobile, Duration::from_secs(60))
            .await
            .unwrap();
        let invite_code = invite.code.clone();
        let credential = store
            .redeem_invite(&invite.code, DeviceRole::Mobile, "phone")
            .await
            .unwrap();
        let db = std::fs::read(dir.path().join("auth.sqlite")).unwrap();
        assert!(!db
            .windows(invite_code.len())
            .any(|w| w == invite_code.as_bytes()));
        assert!(!db
            .windows(credential.secret.len())
            .any(|w| w == credential.secret.as_bytes()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn open_rejects_symlink_database_paths_without_touching_target() {
        use std::os::unix::fs::symlink;
        use std::os::unix::fs::PermissionsExt;

        let dir = private_tempdir();
        let target = dir.path().join("target.sqlite");
        drop(
            NodeAuthStore::open(&target, vec!["fleet.local".into()])
                .await
                .unwrap(),
        );
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        let link = dir.path().join("auth.sqlite");
        symlink(&target, &link).unwrap();

        let result = NodeAuthStore::open(&link, vec!["fleet.local".into()]).await;
        assert!(result.is_err(), "database symlinks must be rejected");
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn open_rejects_nonregular_database_paths_and_symlink_parents() {
        use std::os::unix::fs::symlink;

        let dir = private_tempdir();
        assert!(NodeAuthStore::open(dir.path(), vec!["fleet.local".into()])
            .await
            .is_err());

        let outside = private_tempdir();
        let parent_link = dir.path().join("linked-parent");
        symlink(outside.path(), &parent_link).unwrap();
        let db = parent_link.join("auth.sqlite");
        assert!(NodeAuthStore::open(db, vec!["fleet.local".into()])
            .await
            .is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn open_rejects_broad_database_parent_without_chmodding_it() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let db = dir.path().join("auth.sqlite");
        let result = NodeAuthStore::open(&db, vec!["fleet.local".into()]).await;
        assert!(matches!(result, Err(AuthError::UnprivateDatabaseParent)));
        assert_eq!(
            std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn relative_database_filename_is_rejected_without_chmodding_working_directory() {
        use std::os::unix::fs::PermissionsExt;
        let current = std::env::current_dir().unwrap();
        let before = std::fs::metadata(&current).unwrap().permissions().mode() & 0o777;
        assert!(matches!(
            NodeAuthStore::open("relative-auth.sqlite", vec!["fleet.local".into()]).await,
            Err(AuthError::UnsafeDatabasePath)
        ));
        let after = std::fs::metadata(current).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            after, before,
            "relative open must not change cwd permissions"
        );
    }

    fn handshake(
        client_config: rustls::ClientConfig,
        server_config: rustls::ServerConfig,
        server_name: &str,
    ) -> Result<(), String> {
        let name = ServerName::try_from(server_name.to_owned()).map_err(|e| e.to_string())?;
        let client =
            ClientConnection::new(Arc::new(client_config), name).map_err(|e| e.to_string())?;
        let server = ServerConnection::new(Arc::new(server_config)).map_err(|e| e.to_string())?;
        let mut client = rustls::Connection::Client(client);
        let mut server = rustls::Connection::Server(server);
        for _ in 0..12 {
            transfer(&mut client, &mut server)?;
            transfer(&mut server, &mut client)?;
            if !connection_is_handshaking(&client) && !connection_is_handshaking(&server) {
                return Ok(());
            }
        }
        Err("TLS handshake did not complete".into())
    }

    fn connection_is_handshaking(connection: &rustls::Connection) -> bool {
        match connection {
            rustls::Connection::Client(connection) => connection.is_handshaking(),
            rustls::Connection::Server(connection) => connection.is_handshaking(),
        }
    }

    fn transfer(from: &mut rustls::Connection, to: &mut rustls::Connection) -> Result<(), String> {
        let mut wire = Vec::new();
        from.write_tls(&mut wire).map_err(|e| e.to_string())?;
        if wire.is_empty() {
            return Ok(());
        }
        to.read_tls(&mut Cursor::new(wire))
            .map_err(|e| e.to_string())?;
        to.process_new_packets().map_err(|e| e.to_string())?;
        Ok(())
    }
}
