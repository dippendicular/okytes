use okytes_core::peer::{Entry, Hash, Manifest};
use okytes_daemon::peer_transfer::{Receiver, Source, CHUNK_SIZE};
use std::{fs, io, path::Path};
use tempfile::tempdir;
use uuid::Uuid;

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

#[test]
fn recursive_transfer_recovers_and_publishes_verified_tree() {
    let source_root = tempdir().unwrap();
    let selected = source_root.path().join("album");
    fs::create_dir_all(selected.join("empty")).unwrap();
    let content = vec![0x5a; CHUNK_SIZE + 17];
    write(&selected.join("nested/song.bin"), &content);
    write(&selected.join("readme.txt"), b"hello");
    let source = Source::open(std::slice::from_ref(&selected)).unwrap();
    assert!(source
        .manifest()
        .entries
        .iter()
        .any(|entry| matches!(entry, Entry::Directory { path } if path == "album/empty")));

    let destination = tempdir().unwrap();
    let id = Uuid::new_v4();
    let receiver = Receiver::open(
        destination.path(),
        id,
        "received",
        source.manifest().clone(),
    )
    .unwrap();
    let first = source.read("album/nested/song.bin", 0).unwrap();
    assert_eq!(first.len(), CHUNK_SIZE);
    receiver.write("album/nested/song.bin", 0, &first).unwrap();
    drop(receiver);

    let receiver = Receiver::open(
        destination.path(),
        id,
        "received",
        source.manifest().clone(),
    )
    .unwrap();
    assert_eq!(
        receiver.offset("album/nested/song.bin").unwrap(),
        CHUNK_SIZE as u64
    );
    let second = source
        .read("album/nested/song.bin", CHUNK_SIZE as u64)
        .unwrap();
    receiver
        .write("album/nested/song.bin", CHUNK_SIZE as u64, &second)
        .unwrap();
    let small = source.read("album/readme.txt", 0).unwrap();
    receiver.write("album/readme.txt", 0, &small).unwrap();
    let published = receiver.publish().unwrap();
    assert_eq!(
        fs::read(published.join("album/nested/song.bin")).unwrap(),
        content
    );
    assert_eq!(
        fs::read(published.join("album/readme.txt")).unwrap(),
        b"hello"
    );
    assert!(published.join("album/empty").is_dir());
}

#[test]
fn sender_rejects_links_and_detects_mutation() {
    let root = tempdir().unwrap();
    let file = root.path().join("payload.bin");
    write(&file, b"original");
    let source = Source::open(std::slice::from_ref(&file)).unwrap();
    write(&file, b"changed!");
    assert_eq!(
        source.read("payload.bin", 0).unwrap_err().kind(),
        io::ErrorKind::Other
    );

    #[cfg(unix)]
    {
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(Source::open(&[link]).is_err());
    }
    #[cfg(windows)]
    {
        let link = root.path().join("link");
        if std::os::windows::fs::symlink_file(&file, &link).is_ok() {
            assert!(Source::open(&[link]).is_err());
        }
    }
}

#[test]
fn receiver_rejects_offsets_overflow_corruption_and_conflicts() {
    let root = tempdir().unwrap();
    let bytes = b"expected";
    let manifest = Manifest::new(vec![Entry::File {
        path: "file.bin".into(),
        size: bytes.len() as u64,
        sha256: Hash::digest(bytes),
    }])
    .unwrap();
    let id = Uuid::new_v4();
    let receiver = Receiver::open(root.path(), id, "done", manifest.clone()).unwrap();
    assert!(receiver.write("file.bin", 1, b"x").is_err());
    assert!(receiver.write("file.bin", 0, &[0; 9]).is_err());
    receiver.write("file.bin", 0, b"corrupt!").unwrap();
    assert!(receiver.publish().is_err());

    let receiver = Receiver::open(root.path(), id, "done", manifest).unwrap();
    assert_eq!(receiver.offset("file.bin").unwrap(), 8);
    fs::write(root.path().join("done"), b"occupied").unwrap();
    assert_eq!(
        Receiver::open(
            root.path(),
            Uuid::new_v4(),
            "done",
            Manifest::new(vec![Entry::Directory {
                path: "folder".into()
            }])
            .unwrap()
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::AlreadyExists
    );
}
