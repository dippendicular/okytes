//! Page resolvers are deliberately separate from media transport. A resolver
//! may perform an authorized site-specific lookup, choose tracks and refresh an
//! expiring plan; `MediaTransport` only consumes the resulting manifest URL.
use crate::MediaTransport;
use okytes_core::{desktop::MediaChoice, error::TransportError};
use std::{future::Future, pin::Pin, sync::Arc};

/// A selected manifest and the choices that led to it. Page URLs and signed
/// player endpoints are not accepted as interchangeable transport inputs.
#[derive(Debug, Clone)]
pub struct ResolvedMedia {
    /// An explicit `hls+https://` or `dash+https://` manifest source.
    pub source: String,
    /// Tracks exposed by the manifest or authorized resolver.
    pub choices: Vec<MediaChoice>,
}

/// Adapter boundary for permitted providers. Implementations own provider
/// policy, authentication, format selection, A/V merge planning and expiry
/// refresh. They must return a manifest source suitable for `MediaTransport`.
pub trait MediaResolver: Send + Sync {
    fn resolve(
        &self,
        page_url: &str,
    ) -> Pin<Box<dyn Future<Output = Result<ResolvedMedia, TransportError>> + Send + '_>>;
}

/// Built-in resolver for a user-supplied HLS or DASH manifest. This is the
/// default adapter; it intentionally does not scrape or resolve site pages.
pub struct ManifestResolver {
    media: Arc<MediaTransport>,
}

impl ManifestResolver {
    pub fn new(media: Arc<MediaTransport>) -> Self {
        Self { media }
    }
}

impl MediaResolver for ManifestResolver {
    fn resolve(
        &self,
        page_url: &str,
    ) -> Pin<Box<dyn Future<Output = Result<ResolvedMedia, TransportError>> + Send + '_>> {
        Box::pin(async move {
            let choices = self.media.inspect(page_url).await?;
            Ok(ResolvedMedia {
                source: page_url.to_owned(),
                choices,
            })
        })
    }
}
