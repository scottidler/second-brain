pub mod alert;
pub mod artifact;
pub mod classify;
pub mod extract;
pub mod fetcher;
pub mod raw;
pub mod summarize;

pub use artifact::{ArtifactStore, FsArtifactStore, MemArtifactStore};
pub use extract::{Extractor, PassthroughExtractor};
pub use fetcher::{BrowserUaFetcher, FabricFetcher, Fetcher, FsCachingFetcher, JinaFetcher, MultiFetcher};
pub use raw::{extract_first_url, persist_fetched_if_staging, run_gate_1, run_gate_2, stage_0_init, write_capture};
pub use summarize::{FabricSummarizer, GATE_2_PARAPHRASE_PATTERNS, Summarizer, detect_paraphrased_block};
