//! Plugin foundation. A plugin is a separate process that talks to the Okytes
//! daemon with its own revocable token. The daemon enforces the permissions
//! granted to that token on every request; the manifest only declares what the
//! plugin asks for, so a user can review it before granting access.
//!
//! This API is a preview: manifest schema 1 may gain fields, and there is no
//! plugin marketplace or automatic plugin launching yet.
use crate::{DaemonClient, Error};
use std::path::Path;

pub use okytes_daemon_api::contract::Permission;
pub use okytes_daemon_api::plugin::{Manifest, MANIFEST};

fn invalid(message: &str) -> Error {
    Error::InvalidConfiguration(format!("plugin: {message}"))
}

/// Connected plugin: its manifest plus a daemon client using its own token.
pub struct Plugin {
    /// Validated manifest.
    pub manifest: Manifest,
    /// Client authorized only for the granted permissions.
    pub client: DaemonClient,
}

impl Plugin {
    /// Connect using `OKYTES_DAEMON_URL` and `OKYTES_PLUGIN_TOKEN_FILE`, the
    /// latter written by `okytes-cli plugin grant`.
    pub fn from_environment(manifest: Manifest) -> Result<Self, Error> {
        let url = std::env::var("OKYTES_DAEMON_URL")
            .map_err(|_| invalid("OKYTES_DAEMON_URL is not set"))?;
        let file = std::env::var_os("OKYTES_PLUGIN_TOKEN_FILE")
            .ok_or_else(|| invalid("OKYTES_PLUGIN_TOKEN_FILE is not set"))?;
        let client = DaemonClient::from_file(url, Path::new(&file))
            .map_err(|_| invalid("daemon URL or plugin token is invalid"))?;
        Ok(Self { manifest, client })
    }

    /// Fail early, with a clear message, when calling outside the declared
    /// permissions. The daemon enforces the same rule independently.
    pub fn require(&self, permission: Permission) -> Result<(), Error> {
        if self.manifest.permissions.contains(&permission) {
            Ok(())
        } else {
            Err(Error::InvalidConfiguration(format!(
                "plugin {} did not declare {permission:?}",
                self.manifest.id
            )))
        }
    }
}
