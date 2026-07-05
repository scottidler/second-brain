# Conference slide (OCR text)

Slide header: "Choosing an Embedding Model: Three Tradeoffs"

Bullet list on the slide:

1. Dimensionality vs. cost. Higher-dimensional embeddings (1024+) capture more
   nuance but cost more to store and are slower to search at scale. bge-small
   (384 dims) is a pragmatic default for a single-machine vault.

2. Context window vs. truncation. Most sentence-embedding models cap at 512
   tokens. Text past the cap is silently dropped, so long documents must be
   chunked before embedding or the tail is lost.

3. Local vs. hosted. Local models (fastembed / candle) have zero per-call cost
   and no egress, but need AVX/GPU for acceptable latency. Hosted models are
   faster on weak hardware but add network dependency and per-token cost.

Footer of the slide: "Rule of thumb: chunk first, embed second, measure always."

Bottom-right watermark: "AI Infra Summit 2026".
