//! DNS-SD discovery is an address hint only. TXT fingerprints are untrusted
//! claims until they match an independently approved native trust record.
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::Serialize;
use std::{
    collections::HashMap,
    io,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::RwLock;

pub const SERVICE_TYPE: &str = "_okytes-peer._tcp.local.";
const STALE_AFTER: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, Serialize)]
pub struct Device {
    pub fullname: String,
    pub name: String,
    pub os: String,
    pub protocol: u16,
    pub fingerprint: String,
    pub addresses: Vec<String>,
    pub port: u16,
}

struct Seen {
    device: Device,
    at: Instant,
}

/// Active advertiser/browser. Dropping it withdraws the service and stops its
/// background thread; discovered data is bounded and expires independently.
pub struct Discovery {
    daemon: ServiceDaemon,
    fullname: String,
    seen: Arc<RwLock<HashMap<String, Seen>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Discovery {
    pub fn start(name: &str, port: u16, fingerprint: &str) -> io::Result<Self> {
        if name.is_empty() || name.len() > 63 || port == 0 || fingerprint.len() != 64 {
            return Err(io::Error::other("invalid discovery configuration"));
        }
        let daemon = ServiceDaemon::new().map_err(io::Error::other)?;
        let host = format!("okytes-{}.local.", &fingerprint[..12]);
        let properties = [
            ("name", name),
            ("os", std::env::consts::OS),
            ("v", "1"),
            ("fp", fingerprint),
        ];
        let info = ServiceInfo::new(SERVICE_TYPE, name, &host, (), port, &properties[..])
            .map_err(io::Error::other)?
            .enable_addr_auto();
        let fullname = info.get_fullname().to_owned();
        daemon.register(info).map_err(io::Error::other)?;
        let events = daemon.browse(SERVICE_TYPE).map_err(io::Error::other)?;
        let seen = Arc::new(RwLock::new(HashMap::new()));
        let records = seen.clone();
        let own = fingerprint.to_owned();
        let task = tokio::spawn(async move {
            while let Ok(event) = events.recv_async().await {
                match event {
                    ServiceEvent::ServiceResolved(info) => {
                        let Some(version) = info
                            .get_property_val_str("v")
                            .and_then(|v| v.parse::<u16>().ok())
                        else {
                            continue;
                        };
                        let Some(fp) = info.get_property_val_str("fp") else {
                            continue;
                        };
                        if version != 1
                            || fp == own
                            || fp.len() != 64
                            || !fp
                                .bytes()
                                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                        {
                            continue;
                        }
                        let mut addresses = info
                            .get_addresses()
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>();
                        addresses.sort();
                        addresses.dedup();
                        if addresses.is_empty() {
                            continue;
                        }
                        let device = Device {
                            fullname: info.get_fullname().to_owned(),
                            name: info
                                .get_property_val_str("name")
                                .unwrap_or("Okytes device")
                                .chars()
                                .take(63)
                                .collect(),
                            os: info
                                .get_property_val_str("os")
                                .unwrap_or("unknown")
                                .chars()
                                .take(24)
                                .collect(),
                            protocol: version,
                            fingerprint: fp.to_owned(),
                            addresses,
                            port: info.get_port(),
                        };
                        let mut records = records.write().await;
                        if records.len() < 128 || records.contains_key(&device.fullname) {
                            records.insert(
                                device.fullname.clone(),
                                Seen {
                                    device,
                                    at: Instant::now(),
                                },
                            );
                        }
                    }
                    ServiceEvent::ServiceRemoved(_, fullname) => {
                        records.write().await.remove(&fullname);
                    }
                    _ => {}
                }
            }
        });
        Ok(Self {
            daemon,
            fullname,
            seen,
            task,
        })
    }

    pub async fn devices(&self) -> Vec<Device> {
        let now = Instant::now();
        let mut seen = self.seen.write().await;
        seen.retain(|_, value| now.duration_since(value.at) <= STALE_AFTER);
        let mut result = seen
            .values()
            .map(|value| value.device.clone())
            .collect::<Vec<_>>();
        result.sort_by(|a, b| a.name.cmp(&b.name).then(a.fullname.cmp(&b.fullname)));
        result
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.task.abort();
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.stop_browse(SERVICE_TYPE);
        let _ = self.daemon.shutdown();
    }
}
