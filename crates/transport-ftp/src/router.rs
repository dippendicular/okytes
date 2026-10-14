//! File-protocol dispatch composed with an application-selected fallback.
use crate::FileTransport;
use okytes_core::{
    transport::{FetchResult, ProbeResult, Transport},
    types::RemoteIdentity,
};
use std::sync::Arc;
use url::Url;
/// Route file schemes to the file adapter and all other supported schemes onward.
pub struct Router {
    /// File transport sharing its per-host bounds across all jobs.
    pub files: Arc<FileTransport>,
    /// Existing HTTP or media transport selected by the application.
    pub fallback: Arc<dyn Transport>,
}
impl Router {
    fn route(&self, url: &Url) -> &dyn Transport {
        if self.files.supports(url.scheme()) {
            self.files.as_ref()
        } else {
            self.fallback.as_ref()
        }
    }
}
impl Transport for Router {
    fn supports(&self, scheme: &str) -> bool {
        self.files.supports(scheme) || self.fallback.supports(scheme)
    }
    fn probe(&self, url: &Url, headers: &[(String, String)]) -> crate::Future<'_, ProbeResult> {
        self.route(url).probe(url, headers)
    }
    fn fetch(
        &self,
        url: &Url,
        headers: &[(String, String)],
        range: Option<(u64, u64)>,
        identity: &RemoteIdentity,
    ) -> crate::Future<'_, FetchResult> {
        self.route(url).fetch(url, headers, range, identity)
    }
}
