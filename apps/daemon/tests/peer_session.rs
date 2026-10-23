use okytes_core::peer::{Direction, State};
use okytes_daemon::{
    peer_identity::Identity,
    peer_session::{receive_one, send_managed},
    peer_transfer::Source,
};
use okytes_persistence::SqliteStore;
use std::{fs, sync::Arc};
use tempfile::tempdir;
use tokio::net::TcpListener;
use uuid::Uuid;

#[tokio::test]
async fn approved_mutual_tls_session_streams_checkpoints_verifies_and_publishes() {
    let root = tempdir().unwrap();
    let sender_dir = root.path().join("sender");
    let receiver_dir = root.path().join("receiver");
    let output = root.path().join("output");
    fs::create_dir_all(&sender_dir).unwrap();
    fs::create_dir_all(&receiver_dir).unwrap();
    fs::create_dir_all(&output).unwrap();
    let sender_identity = Identity::open(&sender_dir).unwrap();
    let receiver_identity = Identity::open(&receiver_dir).unwrap();
    let sender_store = SqliteStore::open(sender_dir.join("profile.sqlite")).unwrap();
    let receiver_store = SqliteStore::open(receiver_dir.join("profile.sqlite")).unwrap();
    let receiver_slot = Uuid::new_v4();
    let sender_slot = Uuid::new_v4();
    sender_store
        .peer_approve(
            receiver_slot,
            receiver_identity.certificate().to_vec(),
            receiver_identity.fingerprint(),
            None,
        )
        .await
        .unwrap();
    receiver_store
        .peer_approve(
            sender_slot,
            sender_identity.certificate().to_vec(),
            sender_identity.fingerprint(),
            None,
        )
        .await
        .unwrap();
    let selected = root.path().join("selected");
    fs::create_dir_all(selected.join("empty")).unwrap();
    let payload = vec![0x91; 700_000];
    fs::write(selected.join("large.bin"), &payload).unwrap();
    fs::write(selected.join("small.txt"), b"peer data").unwrap();
    let source = Arc::new(Source::open(std::slice::from_ref(&selected)).unwrap());
    let request_id = Uuid::new_v4();
    let outbound = sender_store
        .peer_offer(
            request_id,
            receiver_identity.fingerprint(),
            Direction::Outbound,
            source.manifest().clone(),
        )
        .await
        .unwrap();
    sender_store
        .peer_transition(
            request_id,
            receiver_identity.fingerprint(),
            outbound.revision,
            State::AwaitingApproval,
        )
        .await
        .unwrap();
    receiver_store
        .peer_offer(
            request_id,
            sender_identity.fingerprint(),
            Direction::Inbound,
            source.manifest().clone(),
        )
        .await
        .unwrap();
    receiver_store
        .peer_transition(
            request_id,
            sender_identity.fingerprint(),
            0,
            State::Negotiating,
        )
        .await
        .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server_tls = receiver_identity
        .server_for_trusted(&receiver_store)
        .await
        .unwrap();
    let receive_store = receiver_store.clone();
    let receive_output = output.clone();
    let receiver = tokio::spawn(async move {
        receive_one(&listener, server_tls, &receive_store, &receive_output).await
    });
    let client_tls = sender_identity
        .client_for(&sender_store, receiver_slot)
        .await
        .unwrap();
    let (sent, received) = tokio::join!(
        send_managed(
            address,
            client_tls,
            &source,
            request_id,
            &sender_store,
            receiver_identity.fingerprint(),
        ),
        receiver
    );
    let state = receiver_store.peer_request(request_id).await.unwrap();
    let progress = receiver_store
        .peer_offsets(request_id, sender_identity.fingerprint())
        .await;
    assert!(
        sent.is_ok(),
        "sender failed: {sent:?}; receiver: {received:?}; state: {state:?}; progress: {progress:?}"
    );
    assert_eq!(received.unwrap().unwrap(), request_id);

    let published = output.join(format!("Okytes transfer {request_id}"));
    assert_eq!(
        fs::read(published.join("selected/large.bin")).unwrap(),
        payload
    );
    assert_eq!(
        fs::read(published.join("selected/small.txt")).unwrap(),
        b"peer data"
    );
    assert!(published.join("selected/empty").is_dir());
    let record = receiver_store
        .peer_request(request_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.state, State::Completed);
    assert_eq!(
        sender_store
            .peer_request(request_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        State::Completed
    );
    let offsets = receiver_store
        .peer_offsets(request_id, sender_identity.fingerprint())
        .await
        .unwrap();
    assert_eq!(offsets.get("selected/large.bin"), Some(&700_000));
    assert_eq!(offsets.get("selected/small.txt"), Some(&9));
}
