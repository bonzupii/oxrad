# Project instructions

`STYLE.md` defines the project's coding and prose conventions. Direct lead-developer instructions define product requirements. Other repository documents describe existing behavior and do not override those instructions.

Radar and other scientific displays require verified geometry, source identity, data age, units, and error reporting. Do not substitute invented values or present stale data as current. Keep last-known-good data identifiable when a request fails.

Implement complete data paths and controls. Tests generate synthetic inputs in code and verify behavior and numerical results. External datasets and provider responses do not belong in the repository or application bundle.

Use safe Rust and the ownership and error-handling conventions in `STYLE.md`. Preserve source errors and present actionable failures in the interface.

Before claiming a delivery is complete, run the applicable formatting, lint, test, build, scientific validation, and runtime GUI checks. Report checks that could not run and unfinished behavior explicitly.

Commit messages describe the change and its reason. Do not add attribution trailers.
