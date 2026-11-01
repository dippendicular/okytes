//! Plugin manifest schema. Plugins are separate processes using their own
//! permission-restricted daemon token; the daemon enforces those permissions on
//! every request. The manifest declares the request so a user can review it.
use crate::contract::Permission;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Manifest file name expected in a plugin's directory.
pub const MANIFEST: &str = "okytes-plugin.json";

/// Declarative `okytes-plugin.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Manifest schema version; currently 1.
    pub schema: u32,
    /// Reverse-domain identifier, for example `org.example.notify`.
    pub id: String,
    /// Display name shown in consent prompts.
    pub name: String,
    /// Plugin version, informational.
    pub version: String,
    /// One-line description shown in consent prompts.
    #[serde(default)]
    pub description: String,
    /// Permissions the plugin requests. Keep this list minimal.
    pub permissions: Vec<Permission>,
}

fn invalid(message: &str) -> String {
    format!("plugin manifest: {message}")
}

impl Manifest {
    /// Read and validate a manifest file or a directory containing one.
    pub fn load(path: &Path) -> Result<Self, String> {
        let file = if path.is_dir() {
            path.join(MANIFEST)
        } else {
            path.to_path_buf()
        };
        let bytes = std::fs::read(&file).map_err(|_| invalid("file cannot be read"))?;
        if bytes.len() > 64 * 1024 {
            return Err(invalid("file exceeds 64 KiB"));
        }
        let manifest: Self =
            serde_json::from_slice(&bytes).map_err(|e| invalid(&format!("invalid JSON: {e}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Reject unknown schemas, unsafe identifiers and duplicate permissions.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != 1 {
            return Err(invalid("unsupported schema; expected 1"));
        }
        let segments: Vec<_> = self.id.split('.').collect();
        if self.id.len() > 100
            || segments.len() < 2
            || segments.iter().any(|s| {
                s.is_empty()
                    || !s
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            })
        {
            return Err(invalid(
                "id must be reverse-domain lowercase, e.g. org.example.tool",
            ));
        }
        for (field, value, limit) in [
            ("name", &self.name, 64),
            ("version", &self.version, 32),
            ("description", &self.description, 200),
        ] {
            if value.len() > limit || value.chars().any(char::is_control) {
                return Err(invalid(&format!(
                    "{field} is too long or has control characters"
                )));
            }
        }
        if self.name.trim().is_empty() || self.version.trim().is_empty() {
            return Err(invalid("name and version are required"));
        }
        let unique: std::collections::HashSet<_> = self.permissions.iter().collect();
        if self.permissions.is_empty() || unique.len() != self.permissions.len() {
            return Err(invalid("list each permission once, and at least one"));
        }
        Ok(())
    }

    /// Token label recorded by the daemon for review and revocation.
    pub fn label(&self) -> String {
        format!("plugin:{}", self.id)
    }

    /// Default token file in an Okytes profile directory.
    pub fn token_path(&self, profile: &Path) -> PathBuf {
        profile.join("plugins").join(format!("{}.token", self.id))
    }
}
