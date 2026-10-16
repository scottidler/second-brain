#![allow(clippy::unwrap_used)]

use super::*;
use std::sync::Arc;

fn ok_stub() -> Stub {
    Arc::new(|_argv: &[String]| Ok(exit(0, "out", "")))
}

#[tokio::test]
async fn a_spawn_inside_a_scope_is_counted_and_answered_by_the_stub() {
    let (output, spawns) = scope(ok_stub(), async {
        let run = yt_dlp(&["--dump-json", "u"]).unwrap();
        run.await.unwrap()
    })
    .await;
    assert!(output.status.success());
    assert_eq!(output.stdout, b"out");
    assert_eq!(spawns.count(), 1);
    assert_eq!(spawns.count_with("--dump-json"), 1);
    assert_eq!(spawns.calls()[0], vec!["--dump-json".to_string(), "u".to_string()]);
}

#[tokio::test]
async fn a_spawn_outside_any_scope_fails_instead_of_starting_a_process() {
    let err = yt_dlp(&["--version"]).err().expect("unscoped spawn must fail");
    assert!(err.to_string().contains("outside a spawn::scope"), "{err}");
}

#[tokio::test]
async fn exit_carries_a_nonzero_code() {
    let (output, _) = scope(Arc::new(|_: &[String]| Ok(exit(1, "", "boom"))), async {
        yt_dlp(&["x"]).unwrap().await.unwrap()
    })
    .await;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stderr, b"boom");
}

/// Two scopes running at once on one runtime each see only their own spawns:
/// the counter is task-local, not a global another test can race.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_scopes_never_see_each_others_spawns() {
    let a = tokio::spawn(scope(ok_stub(), async {
        for _ in 0..3 {
            yt_dlp(&["a"]).unwrap().await.unwrap();
            tokio::task::yield_now().await;
        }
    }));
    let b = tokio::spawn(scope(ok_stub(), async {
        for _ in 0..5 {
            yt_dlp(&["b"]).unwrap().await.unwrap();
            tokio::task::yield_now().await;
        }
    }));
    let ((), spawns_a) = a.await.unwrap();
    let ((), spawns_b) = b.await.unwrap();
    assert_eq!(spawns_a.count(), 3);
    assert_eq!(spawns_b.count(), 5);
    assert_eq!(spawns_a.count_with("b"), 0);
}
