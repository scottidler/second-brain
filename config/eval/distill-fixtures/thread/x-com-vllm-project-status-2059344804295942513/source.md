# Post by vLLM (@vllm_project)

🦀 The Rust frontend is officially merged into vLLM!

As GPUs get faster, the frontend has become a real share of CPU time. The new Rust frontend is a drop-in alternative to the Python API server — same engine, same ZMQ boundary. Opt in with VLLM_USE_RUST_FRONTEND=1.

Early numbers: on a preprocess-heavy workload, ~837 req/s vs ~162 req/s for default Python — ~5x in a single process.

A few design choices we're excited about:
• Layered crates with clear boundaries
• Stream-native pipeline — non-streaming for free
• Builds on stable Rust

Huge thanks to @BugenZhao  from @inferact for introducing the work at @PyTorch Meetup Singapore.

https://github.com/vllm-project/vllm/pull/40848
