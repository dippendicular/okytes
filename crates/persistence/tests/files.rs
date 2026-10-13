use okytes_core::files::PartialFiles;
use okytes_persistence::files::LocalFiles;
#[tokio::test]
async fn partial_creation_refuses_existing_file_and_reopens_regular_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("partial");
    let files = LocalFiles;
    drop(files.create(&path).await.unwrap());
    assert!(files.create(&path).await.is_err());
    assert!(files.open(&path).await.is_ok());
    assert!(files.open(dir.path()).await.is_err());
}
#[cfg(unix)]
#[tokio::test]
async fn partial_symlink_is_refused_and_permissions_are_private() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    let link = dir.path().join("link");
    let files = LocalFiles;
    drop(files.create(&target).await.unwrap());
    assert_eq!(
        std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o600
    );
    symlink(&target, &link).unwrap();
    assert!(files.open(&link).await.is_err());
    assert!(files.create(&link).await.is_err());
}
