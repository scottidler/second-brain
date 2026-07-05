# Distillation eval fixtures

Golden `(source, distilled)` pairs for `sb borg eval`. Each fixture directory
holds:

- `source.md` — the verbatim source text the distiller saw (a fetched article,
  a repo README, a thread, a video transcript, an image description, or a voice
  transcript).
- `distilled.yml` — the `vault::distilled::Distilled` artifact being scored
  against that source.

The eval judge (`judge-distillation.md`) scores each pair on three axes
(claim coverage / anchor validity / summary faithfulness, 0-3) and reports the
mean composite per kind. These fixtures are snapshotted into the repo so they
survive the 60-day staging retention sweep.

## Provenance

- `article/`, `repo/`, `thread/` — snapshotted from real staged traces
  (`~/.local/share/sb/borg/stages/<trace>/{fetched.html,transcript.md} +
  distilled.yml`). Curated, not bulk-dumped: a handful per kind, with a
  long-transcript exemplar deliberately included (the 191 KB `theregister` /
  167 KB video sources).
- `video/` — snapshotted from published vault YouTube notes: `## Transcript`
  becomes `source.md`; `## Summary` + `## Claims` (with timestamp anchors)
  become `distilled.yml`. Videos are harvested this way because youtube
  transcripts are not durably staged (only the published note keeps them).
- `image/`, `voicenote/`, `idea/` — SYNTHETIC, hand-authored, non-personal.
  Voicenote fixtures are synthesized so no personal audio transcript lands in
  the repo (design requirement); image fixtures are synthesized for the same
  privacy reason. Their sources are deliberately richer than their
  distillations so claim coverage below 3 is achievable and measurable.
