use okytes_core::{
    desktop::FileCredentials,
    engine::Engine,
    service::DownloadService,
    state::JobState,
    store::JobStore,
    types::{ConflictStrategy, DownloadRequest, EngineConfig},
};
use okytes_persistence::{files::LocalFiles, SqliteStore};
use okytes_test_support::file_server::{FileServer, Remote};
use okytes_transport_ftp::FileTransport;
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};
#[tokio::test]
async fn ftp_ftps_sftp_engine_resume_keeps_credentials_out_of_sqlite() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    for scheme in ["ftp", "ftps", "sftp"] {
        let temp = tempfile::tempdir().unwrap();
        let bytes = (0..1024 * 1024)
            .map(|n| (n % 251) as u8)
            .collect::<Vec<_>>();
        let remote = Remote::new(bytes.clone());
        let server = if scheme == "sftp" {
            FileServer::sftp(remote.clone(), None).await.unwrap()
        } else {
            FileServer::ftp(remote.clone(), scheme == "ftps")
                .await
                .unwrap()
        };
        let config = EngineConfig {
            database_path: temp.path().join("jobs.db"),
            fixed_concurrency: Some(1),
            ..Default::default()
        };
        let mut roots = rustls::RootCertStore::empty();
        if let Some(cert) = server.certificate.clone() {
            roots.add(cert).unwrap();
        }
        let transport = Arc::new(
            FileTransport::with_roots(&config, temp.path().join("known_hosts"), roots).unwrap(),
        );
        let store = Arc::new(SqliteStore::open(&config.database_path).unwrap());
        let engine = Arc::new(
            Engine::new(
                config.clone(),
                transport.clone(),
                store.clone(),
                Arc::new(LocalFiles),
            )
            .unwrap(),
        );
        let credentials = FileCredentials {
            username: "user".into(),
            password: "password".into(),
            confirmed_host_key: server.fingerprint.clone(),
        };
        let url = format!("{scheme}://localhost:{}/file.bin", server.address.port())
            .parse()
            .unwrap();
        let id = engine
            .add(DownloadRequest {
                url,
                output_dir: temp.path().into(),
                filename: Some(format!("{scheme}.bin")),
                expected_hash: Some(format!("{:x}", Sha256::digest(&bytes))),
                conflict_strategy: ConflictStrategy::Fail,
                headers: credentials.clone().into_headers(),
                priority: 0,
            })
            .await
            .unwrap();
        let worker = tokio::spawn({
            let engine = engine.clone();
            async move { engine.execute_job(id).await }
        });
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            if engine.status(id).await.unwrap().bytes_downloaded >= 8192 {
                break;
            }
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        engine.pause(id).await.unwrap();
        worker.await.unwrap().unwrap();
        let durable = engine.status(id).await.unwrap().bytes_downloaded;
        assert!(durable > 0);
        assert_eq!(store.load_job(id).await.unwrap().unwrap().headers.len(), 0);
        drop(engine);
        let engine = Engine::new(config, transport, store.clone(), Arc::new(LocalFiles)).unwrap();
        engine.authorize_file(id, credentials).await.unwrap();
        engine.resume(id).await.unwrap();
        engine.execute_job(id).await.unwrap();
        assert_eq!(engine.status(id).await.unwrap().state, JobState::Completed);
        assert_eq!(
            std::fs::read(temp.path().join(format!("{scheme}.bin"))).unwrap(),
            bytes
        );
        assert!(remote.offsets.lock().await.iter().any(|n| *n >= 8192));
    }
}
