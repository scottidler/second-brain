# Voice note transcript (synthetic, non-personal)

[00:00] Okay, quick thought while I'm walking. I keep hitting the same slow
query in the retrieval path and I think the fix is a small cache.

[00:12] The issue is that every knowledge search re-embeds the query from
scratch, even when the exact same query string comes in twice in a session.
Embedding a short query is cheap but not free, and on the AVX-only box it's
noticeable.

[00:31] So the idea is a tiny in-memory LRU keyed on the query string, mapping
to the already-computed embedding vector. Cap it at maybe a couple hundred
entries. It doesn't need to persist across restarts.

[00:52] The subtle part is invalidation. If the active embedding model changes,
the cache has to be dropped, because a vector from the old model is garbage
against the new one. So the cache key should really include the model id, not
just the query text.

[01:18] One more thing — I should measure before building this. Add a counter
for how many query embeddings we compute per session first. If it's tiny, the
cache isn't worth the complexity. Measure first, then decide.

[01:40] Also don't cache the full search results, just the query embedding. The
index changes underneath us as the vault updates, so cached result lists would
go stale. The embedding of a fixed query string never goes stale unless the
model changes.
