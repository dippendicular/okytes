use okytes_core::peer::*;
use proptest::prelude::*;
use uuid::Uuid;

fn file(path: &str, size: u64) -> Entry {
    Entry::File {
        path: path.into(),
        size,
        sha256: Hash::digest(b"content"),
    }
}
fn offer() -> Envelope {
    Envelope {
        version: VERSION,
        message: Message::Offer {
            request_id: Uuid::new_v4(),
            manifest: Manifest::new(vec![file("hello.txt", 7)]).unwrap(),
        },
    }
}

#[test]
fn manifest_preserves_empty_directories_unicode_and_large_file_lengths() {
    let manifest = Manifest::new(vec![
        Entry::Directory {
            path: "খালি".into()
        },
        file("বই.txt", 5 * 1024 * 1024 * 1024),
        file("empty", 0),
    ])
    .unwrap();
    assert_eq!(manifest.validate().unwrap(), 5 * 1024 * 1024 * 1024);
    assert_eq!(
        serde_json::from_str::<Manifest>(&serde_json::to_string(&manifest).unwrap()).unwrap(),
        manifest
    );
}

#[test]
fn dangerous_and_ambiguous_paths_are_rejected_on_every_platform() {
    for path in [
        "",
        "/etc/passwd",
        "../a",
        "a/../b",
        "a//b",
        "a/./b",
        "C:/file",
        "\\\\host\\share",
        "a\\b",
        "file:stream",
        "a/",
        "file.",
        "file ",
        "NUL.txt",
        "aux",
        "COM1",
        "Lpt².txt",
        "CON .txt",
        "x\0y",
        "line\ny",
        "a\u{202e}txt",
        "e\u{301}.txt",
    ] {
        assert!(path_key(path).is_err(), "accepted unsafe path {path:?}");
    }
    assert!(path_key(&"a".repeat(256)).is_err());
    assert!(path_key(&vec!["a"; 33].join("/")).is_err());
    for safe in ["é.txt", "CONifer.txt", "COM10", "খালি", "folder/file.txt"] {
        assert!(path_key(safe).is_ok());
    }
}

#[test]
fn collisions_implicit_parents_and_file_as_directory_fail() {
    for entries in [
        vec![file("Readme", 1), file("README", 1)],
        vec![file("a", 1), file("a", 1)],
        vec![file("x/y", 1)],
        vec![file("x", 1), file("x/y", 1)],
        vec![Entry::Directory { path: "X".into() }, file("x/y", 1)],
        vec![file("straße", 1), file("STRASSE", 1)],
        vec![file("é", 1), file("e\u{301}", 1)],
        vec![file("Ａ", 1), file("A", 1)],
    ] {
        assert!(Manifest::new(entries).is_err());
    }
    assert!(Manifest::new(vec![Entry::Directory { path: "x".into() }, file("x/y", 1)]).is_ok());
}

#[test]
fn limits_and_manifest_commitments_are_enforced() {
    assert!(Manifest::new(vec![]).is_err());
    assert!(Manifest::new(vec![file("huge", MAX_BYTES + 1)]).is_err());
    assert!(Manifest::new(vec![file("a", MAX_BYTES), file("b", u64::MAX)]).is_err());
    assert!(Manifest::new(
        (0..=MAX_ENTRIES)
            .map(|n| file(&format!("file{n}"), 1))
            .collect()
    )
    .is_err());
    let first = Manifest::new(vec![file("a", 2), file("b", 3)]).unwrap();
    let second = Manifest::new(vec![file("b", 3), file("a", 2)]).unwrap();
    assert_eq!(first.commitment().unwrap(), second.commitment().unwrap());
    let changed = Manifest::new(vec![file("a", 3), file("b", 3)]).unwrap();
    assert_ne!(first.commitment().unwrap(), changed.commitment().unwrap());
    assert!(Hash::try_from("A".repeat(64)).is_err());
    assert!(Hash::try_from("0".repeat(63)).is_err());
}

#[test]
fn framing_rejects_unsupported_unknown_trailing_and_truncated_input() {
    let value = offer();
    let wire = encode_frame(&value).unwrap();
    assert_eq!(decode_frame(&wire).unwrap(), value);
    for end in 0..wire.len() {
        assert!(decode_frame(&wire[..end]).is_err());
    }
    let mut trailing = wire.clone();
    trailing.push(0);
    assert!(decode_frame(&trailing).is_err());
    assert!(frame_length(0u32.to_be_bytes()).is_err());
    assert!(frame_length(u32::MAX.to_be_bytes()).is_err());
    let mut future = value.clone();
    future.version = VERSION + 1;
    assert!(encode_frame(&future).is_err());
    let nil = Envelope {
        version: VERSION,
        message: Message::Status {
            request_id: Uuid::nil(),
            state: State::Created,
            revision: 0,
        },
    };
    assert!(encode_frame(&nil).is_err());
    let huge_revision = Envelope {
        version: VERSION,
        message: Message::Status {
            request_id: Uuid::new_v4(),
            state: State::Created,
            revision: u64::MAX,
        },
    };
    assert!(encode_frame(&huge_revision).is_err());
    for json in [
        r#"{"version":1,"extra":true,"message":{"type":"status","request_id":"11111111-1111-4111-8111-111111111111","state":"created","revision":0}}"#,
        r#"{"version":2,"message":{"type":"status","request_id":"11111111-1111-4111-8111-111111111111","state":"created","revision":0}}"#,
        r#"{"version":1,"message":{"type":"offer","request_id":"11111111-1111-4111-8111-111111111111","manifest":{"entries":[{"kind":"symlink","path":"x","target":"/"}]}}}"#,
    ] {
        let mut frame = (json.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(json.as_bytes());
        assert!(decode_frame(&frame).is_err());
    }
}

#[test]
fn lifecycle_cannot_skip_approval_or_verification_or_reopen_terminal_states() {
    use State::*;
    assert!(Created.transition(Transferring).is_err());
    assert!(AwaitingApproval.transition(Transferring).is_err());
    assert!(Transferring.transition(Completed).is_err());
    let states = [
        Created,
        AwaitingApproval,
        Negotiating,
        Transferring,
        Verifying,
        Completed,
        Rejected,
        Paused,
        Interrupted,
        Retrying,
        Failed,
        Cancelled,
    ];
    for state in states {
        assert_eq!(state.transition(state).unwrap(), state);
    }
    for terminal in [Completed, Rejected, Cancelled] {
        for state in states {
            assert_eq!(terminal.transition(state).is_ok(), terminal == state);
        }
    }
    for path in [
        vec![
            Created,
            AwaitingApproval,
            Negotiating,
            Transferring,
            Verifying,
            Completed,
        ],
        vec![AwaitingApproval, Rejected],
        vec![
            Transferring,
            Interrupted,
            Retrying,
            Negotiating,
            Transferring,
            Paused,
            Retrying,
            Negotiating,
            Transferring,
            Verifying,
            Failed,
            Retrying,
            Cancelled,
        ],
    ] {
        for step in path.windows(2) {
            assert_eq!(step[0].transition(step[1]).unwrap(), step[1]);
        }
    }
}

#[test]
fn failure_retry_and_pause_cannot_route_around_approval() {
    use State::*;
    let states = [
        Created,
        AwaitingApproval,
        Negotiating,
        Transferring,
        Verifying,
        Completed,
        Rejected,
        Paused,
        Interrupted,
        Retrying,
        Failed,
        Cancelled,
    ];
    let mut reachable = vec![Created];
    let mut cursor = 0;
    while cursor < reachable.len() {
        let current = reachable[cursor];
        cursor += 1;
        for next in states {
            if current == AwaitingApproval && next == Negotiating {
                continue;
            }
            if current.transition(next).is_ok() && !reachable.contains(&next) {
                reachable.push(next);
            }
        }
    }
    for protected in [
        Negotiating,
        Transferring,
        Verifying,
        Completed,
        Paused,
        Interrupted,
        Retrying,
        Failed,
    ] {
        assert!(
            !reachable.contains(&protected),
            "approval bypass reaches {protected:?}"
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]
    #[test]
    fn arbitrary_frames_and_paths_never_panic(bytes in prop::collection::vec(any::<u8>(),0..4096),path in ".{0,1200}") {
        let _=decode_frame(&bytes); let _=path_key(&path);
    }
    #[test]
    fn valid_offers_roundtrip_without_changing_commitment(size in 0u64..MAX_BYTES, name in "[a-z]{1,30}") {
        let manifest=Manifest::new(vec![file(&format!("file-{name}"),size)]).unwrap();
        let value=Envelope { version:VERSION,message:Message::Offer {request_id:Uuid::new_v4(),manifest} };
        prop_assert_eq!(decode_frame(&encode_frame(&value).unwrap()).unwrap(),value);
    }
}
