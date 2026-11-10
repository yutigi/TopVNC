# Plan

1. Inspect the accepted reference source and palette, preserving it unchanged.
2. Restructure the production viewer DOM while retaining stable input IDs and
   the existing Rust/WASM/Worker session boundaries.
3. Apply reference styles; use a full-width desktop display with no touch deck
   at >=900px with a fine primary pointer and hover. Preserve touch layouts.
4. Add persistent validated pinned shortcuts and preserve composition/input
   ownership; verify interaction regressions and viewport bounds.
5. Create a separate desktop design prototype from the reference visual system,
   validate it, and publish it under a new Sites identity.
6. Run web/build/WASM/Worker and Rust workspace checks; record results and limits.
7. Apply the user's desktop correction to production and the separate prototype;
   verify saved modes/placements, hardware input and coarse-pointer landscape,
   then republish only the desktop Site with its existing identity and audience.
