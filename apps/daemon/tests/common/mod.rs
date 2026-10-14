use okytes_daemon::{
    server::{router, Api, Policy},
    transport::Gate,
};
use okytes_daemon_api::{contract::Connection, Client};
use std::{sync::Arc, time::Duration};
pub struct Fixture {
    pub root: tempfile::TempDir,
    pub api: Arc<Api>,
    pub url: String,
    pub token: String,
    handle: axum_server::Handle<std::net::SocketAddr>,
    task: tokio::task::JoinHandle<()>,
}
impl Fixture {
    pub async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let api = tokio::time::timeout(
            Duration::from_secs(15),
            Api::open(
                root.path(),
                &root.path().join("downloads"),
                Policy {
                    hosts: vec![addr.to_string()],
                    origins: vec![],
                },
            ),
        )
        .await
        .expect("API provisioning timeout")
        .unwrap();
        let token = String::from_utf8(
            okytes_browser_bridge::read_secret(&root.path().join("daemon-admin.token")).unwrap(),
        )
        .unwrap();
        let handle = axum_server::Handle::new();
        let stop = handle.clone();
        let router = router(api.clone());
        let task = tokio::spawn(async move {
            let mut server = axum_server::from_tcp(listener)
                .unwrap()
                .acceptor(Gate::new(64))
                .handle(stop);
            server
                .http_builder()
                .http1()
                .timer(hyper_util::rt::TokioTimer::new())
                .header_read_timeout(Duration::from_millis(500))
                .max_headers(32)
                .max_buf_size(32768);
            server.serve(router.into_make_service()).await.unwrap();
        });
        Self {
            root,
            api,
            url: format!("http://{addr}/"),
            token,
            handle,
            task,
        }
    }
    pub fn client(&self, token: String) -> Client {
        Client::new(Connection {
            url: self.url.clone(),
            token,
        })
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.handle.shutdown();
        self.task.abort();
    }
}
