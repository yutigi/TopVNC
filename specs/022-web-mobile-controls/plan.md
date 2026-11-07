# Implementation plan

Use small browser modules for input ownership, validated language preferences and
the mobile deck. Adapt the wireframe customization and language gesture modules;
replace simulated FPS feedback with real key/button/motion callbacks. Mount the
deck beside the existing screen; retain session settings and connection handling.
Use the existing native text adapter and session input/backpressure path. Avoid
new dependencies, external assets or transport/protocol changes.

The project constitution is an unadopted placeholder; AGENTS.md governs this work.

1. Record scope; trace session and input boundaries.
2. Add bounded preferences, chord/hold ownership and input tests.
3. Integrate mobile deck, languages, customization and FPS with lifecycle release.
4. Add interaction tests, document behavior, run required checks and commit.
