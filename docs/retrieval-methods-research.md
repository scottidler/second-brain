# Retrieval Methods for Ingested Second-Brain Data

Research briefing on the current state of retrieval over ingested data: RAG, GraphRAG, BM25, and hybrid approaches.

- **Generated:** 2026-06-14 (via `/last30days`)
- **Window:** 2026-05-16 to 2026-06-15
- **Sources:** 52 items across Reddit (10), Hacker News (14), GitHub (19), Web (9) + targeted web supplements
- **Top voices:** r/Rag, r/PKMS, r/LangChain, infiniflow/ragflow, Hacker News

## TL;DR

For a second brain, **start with hybrid retrieval (BM25 + dense vectors fused via RRF) plus a reranker**, and only reach for GraphRAG where queries genuinely require multi-hop reasoning across scattered notes. The 2026 consensus is **adaptive routing**: classify each query and send simple lookups to cheap hybrid search, reserving graph traversal for relationship-heavy questions. Chunking strategy and retrieval observability matter more than embedding-model choice.

## Findings

### Hybrid retrieval is the settled default, and BM25 refuses to die

Nobody serious runs pure vector search anymore. The pattern is BM25 (exact keyword) plus dense vectors fused with Reciprocal Rank Fusion (RRF), then a reranker on top. A production [GitHub PR](https://github.com/tabularasa31/ai-chatbot/pull/696) even runs "knowledge-base retrieval (BM25 + vector search) concurrently with the relevance guard" to hide the 150-500ms search latency behind the LLM call.

The reasoning: dense retrieval understands meaning and paraphrasing but whiffs on exact IDs, acronyms, and technical terms — exactly what BM25 nails. Published benchmarks back this up: hybrid lifts Recall from ~0.72 (BM25 alone) to ~0.91, and Precision from ~0.68 to ~0.87. Notably, BM25 still outperforms even strong commercial embeddings (`text-embedding-3-large`) on most metrics except deep recall.

### GraphRAG is the most-discussed idea and the most-warned-against

GraphRAG dominates the GitHub and web evidence. A [pydelhi/talks](https://github.com/pydelhi/talks/issues/422) pitch calls it "a new and better approach," and r/Rag's top thread asks ["best way to build a knowledge graph from a vector database?"](https://www.reddit.com/r/Rag/comments/1tugebm/best_way_to_build_a_knowledge_graph_from_a_vector/) (27 pts, 13 comments). GraphRAG works by retrieving a subgraph of relevant entities, their relationships, connected chunks, and community summaries, following multi-hop paths across documents.

But practitioners are blunt about cost. GraphRAG-Bench (ICLR 2026) found it "frequently underperforms vanilla RAG" on simple lookups (~72% local-fact accuracy), while costing **6-8x more to index and ~3x more to operate**. The honest framing from [Graph Praxis](https://medium.com/graph-praxis/the-graphrag-cost-cliff-how-33-000-became-33-in-eighteen-months-be1b0fbe37e4): many teams "looked at the bill and put it back on the shelf."

### The reconciliation everyone lands on is adaptive routing

GraphRAG's clear win is multi-hop reasoning across scattered facts (~91% vs 34-58% for alternatives). So the emerging consensus isn't "pick one" but "route each query": a classifier sends simple factual lookups to cheap hybrid search and reserves graph traversal for relationship-heavy questions. For a second brain this matters — most queries are "find that note," not "synthesize across 40 notes." See [Knowledge Graph vs RAG: When Each One Wins](https://atlan.com/know/knowledge-graphs-vs-rag-for-ai/).

### Local-first second brains are shipping, but small models hallucinate connections

A [Show HN](https://news.ycombinator.com/item?id=48248801) post ("I built a RAG and knowledge graph agent that runs locally") captures the DIY energy. The Obsidian + Ollama crowd is the most practical voice: per [XDA](https://www.xda-developers.com/i-built-a-second-brain-using-only-obsidian-and-a-local-llm/), 7B models "occasionally hallucinate connections between notes that don't exist," so always verify against the source note the tool cites. Tooling mentioned: Copilot (Logan Yang) and Smart Connections (splits by headings by default).

### Verifiability and chunking are the unglamorous bottlenecks

The most telling support thread is an [infiniflow/ragflow issue](https://github.com/infiniflow/ragflow/issues/15889) titled "how to confirm that a knowledge graph has been used in the retrieval" — people literally can't tell if their fancy pipeline is firing. And on a 10,000+ note vault, retrieval quality lives or dies on chunk strategy (split-by-headings is the common default) far more than on which embedding model you pick.

## Key Patterns

1. **Hybrid first.** BM25 + dense + RRF + reranker before anything exotic. (per r/Rag)
2. **GraphRAG is situational.** Wins multi-hop relationship queries; loses on cost and simple lookups. (per Hacker News, GraphRAG-Bench)
3. **Adaptive RAG is the sweet spot.** A query router choosing the pipeline gives the best cost-quality tradeoff. (per atlan.com)
4. **Local Obsidian + Ollama is mature enough** for a personal second brain, with source-citation verification mandatory. (per r/PKMS, XDA)
5. **Observability + chunking > embedding choice.** Make retrieval inspectable and tune chunk size first. (per infiniflow/ragflow)

## Implications for second-brain

- The pragmatic retrieval stack here is **hybrid (BM25 + vector + RRF) + reranker** as the baseline path, with an optional graph layer gated behind a query classifier rather than applied to every query.
- Prioritize **retrieval observability** (log which path/index served each query and which chunks were returned) — the RAGFlow thread shows this is a real, common failure to even detect whether graph retrieval fired.
- Treat **chunking strategy as a first-class tunable**; split-by-heading is the sensible default for note vaults.

## Sources

- Reddit: [r/Rag — KG from vector DB](https://www.reddit.com/r/Rag/comments/1tugebm/best_way_to_build_a_knowledge_graph_from_a_vector/), r/PKMS, r/LangChain
- Hacker News: [Show HN — local RAG + knowledge graph agent](https://news.ycombinator.com/item?id=48248801)
- GitHub: [tabularasa31/ai-chatbot PR (BM25+vector concurrency)](https://github.com/tabularasa31/ai-chatbot/pull/696), [infiniflow/ragflow (confirm KG used)](https://github.com/infiniflow/ragflow/issues/15889), [pydelhi/talks (GraphRAG)](https://github.com/pydelhi/talks/issues/422)
- Web: [atlan.com — KG vs RAG](https://atlan.com/know/knowledge-graphs-vs-rag-for-ai/), [stackviv.ai — GraphRAG guide](https://stackviv.ai/blog/graphrag-knowledge-graphs-rag), [AWS — What is RAG](https://aws.amazon.com/what-is/retrieval-augmented-generation/), [Graph Praxis — GraphRAG cost cliff](https://medium.com/graph-praxis/the-graphrag-cost-cliff-how-33-000-became-33-in-eighteen-months-be1b0fbe37e4), [XDA — Obsidian + local LLM second brain](https://www.xda-developers.com/i-built-a-second-brain-using-only-obsidian-and-a-local-llm/)
- Raw research dump: `~/Documents/Last30Days/data-retrieval-from-ingested-data-in-a-second-brain-rag-graphrag-bm25-raw-v3.md`
