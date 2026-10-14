use okytes_core::{
    desktop::FileCredentials,
    engine::Engine,
    service::DownloadService,
    state::JobState,
    types::{ConflictStrategy, DownloadRequest, EngineConfig},
};
use okytes_persistence::{files::LocalFiles, SqliteStore};
use okytes_test_support::file_server::{FileServer, Remote};
use okytes_transport_ftp::FileTransport;
use std::sync::Arc;
#[tokio::test]
async fn rejected_logins_fail_without_retrying_credentials() {
    for scheme in ["ftp", "sftp"] {
        let temp = tempfile::tempdir().unwrap();
        let remote = Remote::new(vec![1u8; 32]);
        let server = if scheme == "ftp" {
            FileServer::ftp(remote, false).await.unwrap()
        } else {
            FileServer::sftp(remote, None).await.unwrap()
        };
        let config = EngineConfig::default();
        let store = Arc::new(SqliteStore::open(temp.path().join("jobs.db")).unwrap());
        let transport =
            Arc::new(FileTransport::new(&config, temp.path().join("known_hosts")).unwrap());
        let engine = Engine::new(config, transport, store, Arc::new(LocalFiles)).unwrap();
        let credentials = FileCredentials {
            username: "user".into(),
            password: "wrong-password".into(),
            confirmed_host_key: server.fingerprint.clone(),
        };
        let id = engine
            .add(DownloadRequest {
                url: format!("{scheme}://{}/file.bin", server.address)
                    .parse()
                    .unwrap(),
                output_dir: temp.path().into(),
                filename: None,
                expected_hash: None,
                conflict_strategy: ConflictStrategy::Fail,
                headers: credentials.into_headers(),
                priority: 0,
            })
            .await
            .unwrap();
        assert!(engine.execute_job(id).await.is_err());
        let job = engine.status(id).await.unwrap();
        assert_eq!(job.state, JobState::Failed);
        assert_eq!(job.retry_count, 0);
        assert!(!job.output_path.exists());
        assert!(!job
            .error_message
            .unwrap_or_default()
            .contains("wrong-password"));
    }
}
