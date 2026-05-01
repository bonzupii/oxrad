# AGENTS.md

This is safety-critical storm chasing software. People stand in open fields with tornadoes bearing down on them, making life-or-death decisions based on what this display shows. A misplaced bin puts a storm in the wrong location. A silently swallowed error shows stale data as current. A frozen screen looks like clear skies when a wedge tornado is two miles away.

Every rule here is absolute. If you believe a rule cannot be followed in a specific situation, stop, leave a comment at the exact location explaining the conflict, and ask for explicit permission before writing a single line that would violate it.

## Gates

All three must pass before any milestone is considered complete. There are no partial completions.

```
nix-shell --run 'cargo clippy -- -D warnings'   # zero warnings
nix-shell --run 'cargo test'                     # all tests pass
nix-shell --run 'cargo test -- <filter>'         # single test
nix-shell --run 'cargo test -- --nocapture'      # with output
```

## Unconditional Bans

No argument makes these acceptable. No permission is available for any of them.

**Panicking:** `unwrap()`, `expect()`, `panic!()`, `unreachable!()`, `todo!()`, `unimplemented!()`, `assert!(false, …)`, `debug_assert!()`

**Unsafe:** `unsafe { }`, `unsafe fn`, `unsafe trait`, `unsafe impl`

**Silent error discard:** `let _ = fallible()`, `.ok()` without explicit handling, `.unwrap_or_default()`, `map_err(|e| e.to_string())`

**Other:** `#[allow(…)]`, `#![allow(…)]`, commented-out code of any kind

If the compiler requires handling a branch you believe is unreachable, handle it gracefully — return an error, surface a message to the user. You may never assert that a branch is unreachable.

## Ownership and Concurrency

`Arc`, `Rc`, `RefCell`, `Mutex`, `RwLock`, `Cell`, `OnceCell`, `LazyLock` — **banned by default.** Permission required before use. When you believe you need one, stop, leave a comment explaining what you are trying to accomplish and why no approved pattern solves it, then ask.

Ownership resolution order: `&T` → `&mut T` → move `T` → shared ownership (ask first). `clone()` to satisfy the borrow checker means the ownership model is wrong — fix the model.

**Approved concurrency pattern:** worker thread owns its data exclusively → produces a completed owned value → sends it via `std::sync::mpsc` or `crossbeam-channel` → UI thread owns all display state exclusively and never shares it. No other cross-thread pattern without explicit permission.

## Error Handling

- Every fallible call must be handled: `?`, `match`, or typed conversion. Discarding a `Result` in any form is forbidden.
- Library code uses `thiserror` enums. Variants carry the invalid value as a field wherever possible.
- Application glue uses `anyhow` at the UI boundary only. Never `Box<dyn Error>`, never `map_err(|e| e.to_string())`.
- Every error must appear in the UI as a human-readable message. Errors written only to stderr or logs are invisible to the user — invisible errors are silent failures, and silent failures are forbidden.
- Error messages use sentence case: `"Failed to fetch radar data for site {site}: {source}"`.

## Code Style and Architecture

**Naming:** No `Manager`, `Service`, `Controller`, `Factory`, `Provider`, `Helper`, `Util`, or `Handler` in type names. Name types after what they are. No Hungarian notation, no `_impl` suffix. Permitted abbreviations: `lat`, `lon`, `az`, `rng`.

**Abstractions:** No `dyn Trait` — use enums for known variant sets. No single-implementor traits. No trait hierarchies. No builder pattern — use plain struct literals. Newtypes only with a validated constructor returning `Result`. No generic parameters where a concrete type compiles.

**Structure:** Edition 2024, never downgrade. One module per logical concern, named files (`radar.rs`, `tiles.rs`) — no `mod.rs`. `main.rs` ≤ 30 lines: argument parsing, app struct construction, `eframe::run_native`. No unlisted dependency without asking. Implement only the current milestone — no stubs, no scaffolding ahead.

**Imports:** `std` → external crates → internal modules, one blank line between groups, one item per line for lists longer than two.

**Comments:** No explanatory comments. Code must be self-explanatory. Only permission-request comments are permitted.

## Radar Data Integrity

No hardcoded geophysical constants in rendering paths. Gate geometry, azimuth spacing, site coordinates, and elevation angle are read from the data for every radial. If the data does not supply a valid value, the bin or radial is rejected and its count is surfaced in the UI — it is never substituted with a default. A bin placed one gate off puts a storm in the wrong location. A storm in the wrong location kills someone who turned the wrong way.

## Coordinate Mathematics

All formulas are locked to the verified implementations in `ARCHITECTURE.md`. Implement them exactly as specified using the given variable names. Earth radius: **6371.0 km**. Authoritative test site: `site_lat = 37.0683`, `site_lon = -88.7720`.

Previously observed bugs that must not be reintroduced: bare ratio instead of `atan2` in the forward longitude calculation; `cos(lat2)` instead of `cos(lat1)` in the `atan2` numerator; missing `/2.0` divisions in the haversine terms; `a.sqrt().asin()` instead of `2.0 * atan2(sqrt(a), sqrt(1-a))`; missing clamp before `asin` and before `sqrt`; missing longitude normalization to `[-180, 180]`.

## Testing

All unconditional bans apply equally to test code. Tests return `Result<(), Box<dyn std::error::Error>>` and propagate failures with `?`.

Every `assert!` includes a message stating the expected value, the actual value, and the tolerance. Error-path tests match on the specific error variant and assert the value it carries — `result.is_err()` alone is not sufficient. Loop-based tests collect all failures and report them together at the end — `continue` on failure is banned. Round-trip tests do not substitute for absolute reference value tests; both are required.

**Banned in tests:** `.ok().unwrap_or(…)`, `assert!(false, …)`, `continue` on failure, custom float-comparison helpers that hide actual values, approximate site coordinates.

**Required coverage floor:** forward and inverse reference values from `ARCHITECTURE.md` within stated tolerances; round-trip for eight azimuths at 75 km from KPAH; all error variants with their carried values; OSM tile forward/inverse at two zoom levels; color map keypoint values and out-of-range error paths.
