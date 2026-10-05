pub mod alert;
pub mod artifact;
pub mod classify;
pub mod distill;
pub mod extract;
pub mod fetcher;
pub mod raw;
pub mod summarize;

#[cfg(test)]
pub use artifact::MemArtifactStore;
pub use artifact::{ArtifactStore, FsArtifactStore};
pub use distill::{DistillStage, distill_kind_from_ingest};
pub use extract::{Extractor, PassthroughExtractor};
pub use fetcher::{BrowserUaFetcher, Fetcher, FsCachingFetcher};
pub use raw::{extract_first_url, persist_fetched_if_staging, run_gate_1, run_gate_2, stage_0_init, write_capture};
pub use summarize::{GATE_2_PARAPHRASE_PATTERNS, detect_paraphrased_block};
