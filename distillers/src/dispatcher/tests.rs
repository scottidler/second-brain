use super::*;

#[tokio::test]
async fn dispatches_idea_to_idea_distiller() {
    let dispatcher = Dispatcher::new();
    let inputs = DistillInputs {
        transcript: "An idea about caches.",
        source_url: None,
        title_hint: None,
    };
    let distilled = dispatcher.distill(DistillKind::Idea, inputs).await.expect("distill");
    assert_eq!(distilled.meta.extractor, "distill-idea-v1");
}

#[tokio::test]
async fn dispatches_image_to_passthrough() {
    let dispatcher = Dispatcher::new();
    let inputs = DistillInputs {
        transcript: "ocr text",
        source_url: None,
        title_hint: None,
    };
    let distilled = dispatcher.distill(DistillKind::Image, inputs).await.expect("distill");
    assert_eq!(distilled.meta.extractor, "distill-passthrough-v1");
}

#[tokio::test]
async fn dispatches_voice_note_to_passthrough() {
    let dispatcher = Dispatcher::new();
    let inputs = DistillInputs {
        transcript: "transcribed audio",
        source_url: None,
        title_hint: None,
    };
    let distilled = dispatcher
        .distill(DistillKind::VoiceNote, inputs)
        .await
        .expect("distill");
    assert_eq!(distilled.meta.extractor, "distill-passthrough-v1");
}

#[tokio::test]
async fn fabric_kinds_bail_until_phase_3_plus() {
    let dispatcher = Dispatcher::new();
    for kind in [
        DistillKind::Article,
        DistillKind::Repo,
        DistillKind::Video,
        DistillKind::Thread,
    ] {
        let inputs = DistillInputs {
            transcript: "x",
            source_url: None,
            title_hint: None,
        };
        let err = dispatcher
            .distill(kind, inputs)
            .await
            .expect_err("phase-2 dispatcher must not handle Fabric kinds yet");
        let msg = format!("{err}");
        assert!(
            msg.contains("Phases 3-6"),
            "error should reference Phases 3-6 for {kind:?}; got {msg}"
        );
    }
}
