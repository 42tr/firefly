# Firefly patch

This is the published `glow-agent` 0.3.4 source (MIT OR Apache-2.0).
The local change in `src/providers/openai.rs` adds `with_reasoning_effort` and
serializes a configured value as Responses `reasoning.effort`. Omitting the
option preserves the upstream request. Streaming, retries, repairs, and context
compaction use the same configured model instance.

Remove this patch when adopting an upstream release with equivalent support.
