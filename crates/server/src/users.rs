//! Account store persisted as JSON. Passwords are stored as
//! SHA-256(salt ‖ password) with a random per-user salt, never in clear.
//! (A production system would use a slow KDF like Argon2 and TLS on the wire.)

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use gg_shared::constants::{PASSWORD_MAX, PASSWORD_MIN, USERNAME_MAX, USERNAME_MIN};
use gg_shared::protocol::AuthErrorReason;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UserRecord {
    id: u32,
    username: String,
    salt: String,
    password_hash: String,
}

pub struct UserStore {
    path: PathBuf,
    users: Vec<UserRecord>,
    /// In-memory bot accounts: never written to disk.
    bots: Vec<UserRecord>,
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hash_password(salt: &str, password: &str) -> String {
    let mut h = Sha256::new();
    h.update(salt.as_bytes());
    h.update(password.as_bytes());
    to_hex(&h.finalize())
}

pub fn valid_username(name: &str) -> bool {
    (USERNAME_MIN..=USERNAME_MAX).contains(&name.chars().count())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !gg_shared::bot::is_bot_name(name) // reserved for server-spawned bots
}

impl UserStore {
    pub fn load(path: &Path) -> io::Result<Self> {
        let users = match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: {e}", path.display()),
                )
            })?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        Ok(UserStore {
            path: path.to_path_buf(),
            users,
            bots: Vec::new(),
        })
    }

    pub fn len(&self) -> usize {
        self.users.len()
    }

    pub fn is_empty(&self) -> bool {
        self.users.is_empty()
    }

    fn find(&self, username: &str) -> Option<&UserRecord> {
        self.users
            .iter()
            .chain(&self.bots)
            .find(|u| u.username.eq_ignore_ascii_case(username))
    }

    pub fn register(&mut self, username: &str, password: &str) -> Result<u32, AuthErrorReason> {
        if !valid_username(username) {
            return Err(AuthErrorReason::InvalidUsername);
        }
        if !(PASSWORD_MIN..=PASSWORD_MAX).contains(&password.chars().count()) {
            return Err(AuthErrorReason::InvalidPassword);
        }
        if self.find(username).is_some() {
            return Err(AuthErrorReason::UserExists);
        }
        let salt = to_hex(&rand::random::<[u8; 16]>());
        let id = self.next_id();
        self.users.push(UserRecord {
            id,
            username: username.to_string(),
            password_hash: hash_password(&salt, password),
            salt,
        });
        if let Err(e) = self.save() {
            // Account still works for this server run; just warn.
            log::error!("could not persist accounts to {}: {e}", self.path.display());
        }
        Ok(id)
    }

    /// Returns `(player_id, canonical username)` on success.
    pub fn verify(&self, username: &str, password: &str) -> Option<(u32, String)> {
        let user = self.find(username)?;
        (hash_password(&user.salt, password) == user.password_hash)
            .then(|| (user.id, user.username.clone()))
    }

    /// Write to a temp file then rename, so a crash never leaves a half-written file.
    fn next_id(&self) -> u32 {
        self.users
            .iter()
            .chain(&self.bots)
            .map(|u| u.id)
            .max()
            .unwrap_or(0)
            + 1
    }

    /// Create an in-memory account for a server-spawned bot. Bot names use
    /// the reserved prefix, so they can't collide with human accounts.
    pub fn add_bot(&mut self, username: &str, password: &str) -> u32 {
        let salt = to_hex(&rand::random::<[u8; 16]>());
        let id = self.next_id();
        self.bots.push(UserRecord {
            id,
            username: username.to_string(),
            password_hash: hash_password(&salt, password),
            salt,
        });
        id
    }

    fn save(&self) -> io::Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(&self.users)?)?;
        fs::rename(tmp, &self.path)
    }
}
