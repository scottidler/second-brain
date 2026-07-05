# Idea note (synthetic, non-personal)

This is how we should fix the vault linker: use the capture annotation as a
seed edge.

When I paste a URL with a sentence like "this is the missing piece for the
retrieval eval work," that sentence names the relationship between the new
source and something already in the vault. Right now that sentence is thrown
away at the transport, so the strongest signal about where the note belongs
never reaches the graph.

The idea: thread the capture note into the distiller as trusted context, and
have the linker treat entities mentioned in the capture note as high-priority
link candidates for the new note. The user literally told us what this connects
to; we should not make the linker re-derive it from scratch.

Risk: the capture note is free text and could be noisy or could mention things
not yet in the vault. So it seeds candidates, it doesn't force edges — the
existing link-confidence gate still applies.
