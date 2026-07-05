# Whiteboard photo (vision description)

A photograph of a whiteboard covered in a hand-drawn systems diagram, titled
"INGEST PIPELINE" across the top in blue marker.

Boxes and arrows, left to right:

- A box labeled "TRANSPORTS (telegram / signal / ntfy / http / cli)" feeds into
- a box labeled "STAGE 0 FETCH (jina, fabric-u, browser-UA)", which feeds into
- a box labeled "STAGE 2 DISTILL — per-kind fabric patterns", which branches:
  - "short input -> single call"
  - "long input -> chunk / map / reduce"
- The distill box feeds a box labeled "STAGE 3 PUBLISH -> vault note".
- Below, a separate box reads "cortex embed: summary | transcript-chunk" with a
  dashed arrow to a cylinder labeled "oracle SQLite (FTS5 + vectors)".

In the bottom-right corner, in red marker and underlined twice:
"CLAIMS ARE INVISIBLE TO VECTOR-ONLY RETRIEVAL — FIX THIS".

A sticky note in the top-left corner reads "durable capture != reachable
capture". Another sticky note reads "measure first: build the eval harness".

There is a small doodle of a robot next to the word "borg" and a coffee-cup
ring stain overlapping the STAGE 2 box.
