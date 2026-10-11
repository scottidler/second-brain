use super::*;

use vault::embedding::MockEmbedder;

fn mock() -> Result<Box<dyn EmbeddingModel>> {
    Ok(Box::new(MockEmbedder::new(8, "mock-startup")))
}

#[test]
fn a_stop_during_the_load_exits_before_the_startup_sweep() {
    let stop = StopHandle::never();
    let outcome = load_unless_stopped(&stop, || {
        stop.stop();
        mock()
    });
    assert!(matches!(outcome, StartupModel::Stopped));
}

#[test]
fn a_stop_during_a_failed_load_still_exits() {
    let stop = StopHandle::never();
    let outcome = load_unless_stopped(&stop, || {
        stop.stop();
        Err(eyre::eyre!("download interrupted"))
    });
    assert!(matches!(outcome, StartupModel::Stopped));
}

#[test]
fn no_stop_hands_back_the_model() {
    let outcome = load_unless_stopped(&StopHandle::never(), mock);
    assert!(matches!(outcome, StartupModel::Ready(Some(_))));
}

#[test]
fn a_failed_load_without_a_stop_disables_only_the_embed_tick() {
    let outcome = load_unless_stopped(&StopHandle::never(), || Err(eyre::eyre!("no model")));
    assert!(matches!(outcome, StartupModel::Ready(None)));
}
