# Firefly patch

This is the published `glow-agent` 0.3.4 source (MIT OR Apache-2.0).
The local change in `src/providers/openai.rs` adds `with_reasoning_effort` and
serializes a configured value as Responses `reasoning.effort`. Omitting the
option preserves the upstream request. Streaming, retries, repairs, and context
compaction use the same configured model instance.

`src/model_limits.rs` also exposes discovered `ModelLimits` (including their
source URL) through `ModelBudget::limits`. Firefly's configuration discovery can
therefore reuse the runtime's provider queries, exact-model official-document
fallback, safe input-budget calculation and persistent metadata cache.
Numeric snapshot aliases may use the parent model's official page only when
their exact ID is documented; limits are scoped to that version's section.

Remove this patch when adopting an upstream release with equivalent support.
