use super::*;

use vault::search::EmbeddingKind::{Claim, Summary, TranscriptChunk};

#[test]
fn no_carry_keeps_the_configured_order() {
    let carry = EmbedCarry::default();
    assert_eq!(
        carry.run_order(vec![Summary, TranscriptChunk, Claim]),
        vec![Summary, TranscriptChunk, Claim]
    );
}

#[test]
fn a_carried_kind_runs_first_and_the_rest_keep_their_cycle() {
    let mut carry = EmbedCarry::default();
    carry.allowance_ended(&[Summary, TranscriptChunk, Claim], 1, 0);
    assert_eq!(carry.lead(), Some(TranscriptChunk));
    assert_eq!(
        carry.run_order(vec![Summary, TranscriptChunk, Claim]),
        vec![TranscriptChunk, Claim, Summary]
    );
}

#[test]
fn a_kind_that_made_progress_hands_the_lead_to_the_next_kind() {
    let mut carry = EmbedCarry::default();
    carry.allowance_ended(&[TranscriptChunk, Summary], 0, 600);
    assert_eq!(carry.lead(), Some(Summary));
}

#[test]
fn progress_by_the_last_kind_carries_nothing() {
    let mut carry = EmbedCarry::default();
    carry.allowance_ended(&[Summary, TranscriptChunk], 1, 300);
    assert_eq!(carry.lead(), None);
}

#[test]
fn a_carried_kind_that_is_not_enabled_changes_nothing() {
    let mut carry = EmbedCarry::default();
    carry.allowance_ended(&[Summary, TranscriptChunk, Claim], 2, 0);
    assert_eq!(
        carry.run_order(vec![Summary, TranscriptChunk]),
        vec![Summary, TranscriptChunk]
    );
}

#[test]
fn clear_drops_the_lead() {
    let mut carry = EmbedCarry::default();
    carry.allowance_ended(&[Summary, TranscriptChunk], 1, 0);
    carry.clear();
    assert_eq!(carry.lead(), None);
}
