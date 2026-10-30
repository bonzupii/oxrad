# oxrad Style Guide

## Purpose and scope

This guide governs how oxrad code and technical prose are written. It complements [ROADMAP.md](ROADMAP.md), which owns product behavior, system architecture, data contracts, runtime guarantees, and the development roadmap.

This guide does not define product scope, decide which features ship, or authorize exceptions to architectural requirements. It does not contain a roadmap. The architecture document does not contain this guide's Rust conventions, naming rules, comment conventions, lint configuration, or prose guidance.

The rules below distinguish hard, mechanically enforceable constraints from engineering principles and contextual style guidance. A hard constraint is absolute. An engineering principle directs implementation and requires a reasoned design. A style principle guides judgment and is applied according to whether it improves clarity and usefulness. Do not turn contextual guidance into a universal prohibition, and do not soften a hard constraint into a preference.

## Hard constraints

### Safe Rust only

The project uses Rust Edition 2024. `unsafe_code = "deny"` is absolute. No unsafe block, unsafe function, unsafe trait implementation, or unsafe operation is permitted anywhere in oxrad, including FFI wrappers and platform boundaries. Do not request approval, propose an exception, or document a justification for violating this rule. A dependency's internal unsafe implementation is assessed during dependency selection and recorded in `docs/DEPENDENCIES.md`; oxrad's own code remains safe Rust.

### Ownership

Shared ownership types are prohibited. Do not use `Arc`, `Rc`, or equivalent reference-counted shared ownership to share application data. Do not use `Arc<Mutex<T>>`, `Arc<RwLock<T>>`, `Rc<RefCell<T>>`, or another wrapper to recreate shared ownership or shared mutable state.

Use ownership transfer, scoped borrows, message passing, and explicit state ownership. The architecture's one-way worker channels are the concurrency model. Each mutable state has one owner. A worker sends owned results to the next stage; it does not expose shared mutable state to other workers or the UI.

A clone is not automatically forbidden by the type system, but cloning to work around an ownership mistake is unacceptable. Prefer shared borrow, mutable borrow, then move. Clone only when an independent owned value is semantically required, such as retaining an immutable snapshot while the original owner continues, and keep the copied data bounded. Do not use comments or documentation to excuse unnecessary cloning.

### No stubs or placeholder implementations

Required functionality is implemented completely to its acceptance criteria. A stub is not a deliverable state. No placeholder body, fake success response, empty event handler, TODO implementation, hardcoded sample substituted for a real data path, dead control, or documentation-only claim may stand in for a required feature.

Do not delete a required feature to make the build, lint, or test suite pass. Fix the code while preserving the feature. Do not mark a feature complete because its symbol, module, control, or test exists. Completion requires the behavior to work in the running application.

Code that is genuinely obsolete, unreachable, or outside the approved product scope is dead weight and is removed. That is not permission to delete unfinished required functionality.

### Mechanical quality gates

The compiler and configured lints are gates. Warnings are denied. `#[allow]` attributes are prohibited. Do not weaken lint configuration, suppress a diagnostic, remove a test, or reduce a feature to obtain a green build. Fix the underlying code.

Production code does not use `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!`, or `unreachable!`. Do not index or slice with computed indices without proving the bounds. Tests fail through assertions and propagated errors.

Fallible operations are never silently discarded. No `let _ = fallible_operation`, no catch-all match arm that hides an unhandled case, and no fallback that converts failure into plausible data.

## Rust design and implementation

### Types first

Make invalid states unrepresentable. Use enums for exclusive states, newtypes for validated values, and typestate where protocol ordering is material. Parse external representations once and carry validated types internally.

Runtime validation belongs at real boundaries: parsing, I/O, deserialization, operating-system interfaces, and provider boundaries. Rechecking a property already guaranteed by a type adds noise unless it protects against a distinct external invariant.

Signatures express borrowing, ownership, and fallibility accurately. If a body contradicts its signature, correct the implementation. Do not accept unvalidated values and then repeatedly rediscover their validity deeper in the call graph.

### Error types

Library modules expose typed error enums. Variants identify the failure mode and retain the structured context that produced it, including the invalid value where reporting it is useful and safe. Implement `Display` and `Error` directly: the message is written for the user, and `source()` carries the cause. Do not collapse distinct failures into `Msg(String)`, `Box<dyn Error>` on public APIs, or `.map_err(|e| e.to_string())`.

Preserve the error chain and structured source information until the point where the error is presented. User-visible errors are human-readable and actionable; logging alone is not error handling.

The architecture document defines application-level failure states, recovery, retry, isolation, and user-visible behavior. This guide defines the Rust representation and propagation of errors. They are complementary responsibilities.

### Fallbacks and required algorithms

Implement the specified algorithm and data path correctly. An incorrect calculation, decoder, data association, or rendering result is a defect to diagnose and fix, not a normal operating mode. Never conceal it with a substitute value, sentinel, alternative algorithm, approximation, warning, retry loop, or plausible-looking output. Stop the affected result from being used, identify and repair the cause, and verify the correction against independent reference values, format specifications, or known-good data. Validation and error reporting contain a defect; they do not repair it.

Distinguish implementation defects from operational conditions such as a timeout, unavailable feed, incomplete transmission, or malformed external data. Retry the same required operation when retry may help and the product's retry policy requires it. Do not endlessly retry deterministic failures on identical inputs or treat a retry as a correctness fix. Preserve last-known-good data only as specified, with its provenance, age, stale state, and the active failure visible; it is not a successful new result.

Honor source-format semantics and the product contract. A format-defined category such as below-threshold or range-folded is data, not evidence of a decoder failure. Reject invalid or missing external data at the gate, radial, record, or volume boundary specified by the format and architecture, with the required diagnostics. Never manufacture a value or reinterpret invalid data as a valid category.

Do not invent fallbacks, including simplified algorithms, hard-coded results, placeholder data, or substitutes that make a failed feature appear functional. Visibility, logging, documentation, and tests do not authorize an invented fallback. A fallback exists only when the product specification independently requires or explicitly authorizes it for a named condition. Preserve the failure context, distinguish the fallback state from ordinary success in the caller or UI, and test both the condition and the specified behavior. Never present a degraded result as successful output from the required algorithm.

Do not use `unwrap_or_default`, `unwrap_or(0)`, or `unwrap_or("")`. Required radar geometry, coordinates, units, timestamps, and measurements are never fabricated. A specified default names the exact condition it handles and remains visible and testable.

Use exhaustive matches for closed sets. A wildcard arm is appropriate only when the domain is genuinely open and the behavior for future or unknown variants is explicit. Do not use a wildcard to suppress compiler guidance.

### Abstraction

Generality is exercised, not anticipated. For a closed set, use an enum and exhaustive `match`. Do not use dynamic trait dispatch merely to represent a closed set. A trait is appropriate for an actual extension boundary, such as source-specific providers or decoders where independent implementations exist or are part of the architecture.

Do not add a builder where a struct literal expresses construction. Do not add generic parameters instantiated at only one type. Remove forwarding layers that make no decision and have no independent responsibility. Delete a layer with no client, including states no producer reaches and unused configuration. Apply the subtraction test: if deleting a layer preserves behavior, tests, and readability, remove it. Complexity must name its client or invariant in a mechanistic comment.

Do not collapse genuine extension points merely to satisfy a simplistic “one implementation” rule. The architecture's source catalog and decoder/provider boundaries are deliberate extensibility points.

### One canonical computation and capability

A derived value with multiple consumers has one canonical computation. Extract shared derivation rather than copying formulas or maintaining parallel implementations.

A feature that needs a capability already owned by a subsystem uses that subsystem. Do not create a parallel storage path, network client, cache, error model, or decoder for the same responsibility. Extend the canonical capability when it lacks a required operation.

### Small units and cohesive modules

God modules, objects, structs, enums, functions, and other oversized or over-responsible components are prohibited. Organize units around cohesive responsibilities and reasons to change. Split units that accumulate unrelated responsibilities or excessive coupling; a line-count lint cannot establish cohesion. Functions past 100 lines, with seven or more arguments, or with cognitive complexity past 15 are factoring tasks, not suppression targets. Factor along responsibilities and reasons to change; do not create meaningless wrappers to satisfy a number. When the last five features land in the same unit for different reasons, factor that unit.

`main.rs` contains argument parsing and application construction. Use named module files rather than `mod.rs`. Keep dependencies behind the module that uses them.

### Effects and state

Make mutation, I/O, time, randomness, and external state visible in APIs. Pure computations remain pure. Avoid hidden global state. Use `static` for compiled-once values and atomics with clear ownership, not as a shortcut for passing state.

Interior mutability is not a workaround for the shared-ownership prohibition. Single-owner mutable state should be represented directly. Cross-thread communication uses owned messages and channels, not shared state.

### Strings and domain types

Strings belong at external and presentation boundaries: file contents, user input, display text, and serialized forms. Internal values receive domain types at parsing: paths use `PathBuf` or `&Path`, fixed key sets use enums, modes and commands use enums, and identifiers use newtypes.

A `String` field is appropriate for human text. It is not an appropriate representation for a fixed mode, command, identifier, or validated quantity. Parse once and carry the type.

### Performance

Prefer borrowing over allocation in hot paths. Use `&str` for borrowed text, `Cow` for conditional ownership when it improves the actual data path, and iterators without unnecessary materialization.

Eager work in lazy positions and O(n) operations where an existing structure offers O(1) are defects; find and verify hot paths by profiling. Measure before optimizing. A performance change must preserve the work being measured and the product behavior. A benchmark names its baseline, workload, hardware, and method. Diverse and adversarial inputs matter. Disabling work, skipping validation, dropping frames silently, or reducing data fidelity is not a performance improvement.

Fixed-size data should use fixed-size representations when that improves memory use and access patterns. Field packing, boxed large enum variants, and compact parallel storage are decisions justified by measured memory or performance behavior, not rules to apply blindly.

### Concurrency

The architecture defines worker ownership and channel flow. Never share mutable state across workers. Do not use stdout printing to investigate races; it changes scheduling and can hide timing defects. Use structured, non-blocking tracing and concurrency tests with reproducible failing schedules.

Counters that require atomic updates use a single atomic operation. A read followed by a write is not an atomic increment. Do not add a lock around a split read/write operation when one atomic operation expresses the intended update.

### Dependency APIs

Check APIs and idioms against the exact dependency version in `Cargo.toml` and `Cargo.lock`. Do not invent methods from memory or use an API that is absent from the locked version. Keep dependency-specific model types inside their adapter module.

### Structure and naming

Use names that describe the domain concept or responsibility. Do not use `Manager`, `Service`, `Controller`, `Factory`, `Provider`, `Helper`, `Util`, or `Handler` in type names.

Do not use Hungarian notation or `_impl` suffixes. Newtypes have validated constructors returning `Result` when construction can fail. Getters return references for borrowed fields; constructors take owned values when the struct stores them.

Imports are explicit. Group `std`, external crates, and internal modules with one blank line between groups. Glob imports are limited to preludes, tests, and intentional re-export modules.

## Tests and verification

Tests assert specified behavior, not mere execution or presence. Assertions identify expected and actual values; floating-point assertions state tolerances. Error-path tests match the specific error variant and relevant carried data. `result.is_err()` alone does not establish correct error behavior.

Tests return `Result` and propagate setup failures with `?`. A test must fail when its target behavior is deliberately broken. Round-trip tests supplement absolute reference-value tests; they do not replace them.

Establish expected behavior from the architecture and source-format contracts before changing a test. When implementation and test are both wrong, correct both against the specification. Never alter an assertion merely to make a failing implementation pass. No exemption lists, skip filters, or `#[ignore]`d tests park known failures; fix the failure or correct the expectation against evidence.

Coverage includes:
- Coordinate transforms against the architecture document's reference values.
- Direct/inverse round trips at eight azimuths and 75 km from the KPAH origin.
- Every error variant and its relevant carried context.
- Tile transforms at two zoom levels.
- Colormap keypoints and out-of-range paths.
- Malformed inputs, truncation, corrupt headers, oversized lengths, and invalid metadata for each decoder.
- Recovery behavior, cancellation, stale-data labeling, worker panic isolation, and independent subsystem continuation.
- Feature acceptance criteria in the running binary.

A green test suite is necessary and not sufficient. Review tests for wrong expectations, unexercised generality, bypassed canonical subsystems, and assertions that encode implementation convenience instead of product behavior.

## Lints

The lint gate is `cargo clippy -- -D warnings` using the project's `[lints]` configuration and `clippy.toml`. Warnings are denied. `#[allow]` is not used. The lint configuration is a mechanical gate, not a substitute for code review or runtime acceptance testing.

The following baseline is retained from the project specification:

```toml
[lints.rust]
dead_code = "deny"
unused_variables = "deny"
unused_imports = "deny"
unused_mut = "deny"
unreachable_code = "deny"
deprecated = "deny"
unsafe_code = "deny"

[lints.clippy]
allow_attributes = "deny"
unwrap_used = "deny"
expect_used = "deny"
panic = "deny"
panic_in_result_fn = "deny"
unwrap_in_result = "deny"
indexing_slicing = "deny"
get_unwrap = "deny"
todo = "deny"
unimplemented = "deny"
unreachable = "deny"
undocumented_unsafe_blocks = "deny"
multiple_unsafe_ops_per_block = "deny"
mem_forget = "deny"
str_to_string = "deny"
string_add = "deny"
cognitive_complexity = "deny"
missing_panics_doc = "warn"
missing_errors_doc = "warn"
needless_pass_by_value = "warn"
trivially_copy_pass_by_ref = "warn"
map_unwrap_or = "warn"
inefficient_to_string = "warn"
format_push_string = "warn"
unnecessary_box_returns = "warn"
too_many_lines = "warn"
struct_excessive_bools = "warn"
struct_field_names = "warn"
large_types_passed_by_value = "warn"
mutex_atomic = "warn"
redundant_clone = "warn"
or_fun_call = "warn"
needless_collect = "warn"
option_if_let_else = "warn"
missing_const_for_fn = "warn"
```

```toml
# clippy.toml
cognitive-complexity-threshold = 15
too-many-arguments-threshold = 7
too-many-lines-threshold = 100
```

The `warn` entries above are build failures because the project runs Clippy with `-D warnings`. Threshold violations require factoring without artificial fragmentation or weaker behavior.

Floating-point computation is expected in radar geometry and meteorology. Do not ban floats. Every computation with known reference values has absolute-value tests with stated tolerances. Do not use `==` or `!=` on computed floating-point values. Format-defined sentinel categories such as `RangeFolded` are represented distinctly in the domain model and must not be confused with an arbitrary NaN. Non-finite decoded measurements are rejected and counted; they are not allowed to contaminate calculations or rendering. Domain clamps needed to protect inverse trigonometric functions from rounding error are explicit and tested.

## Comments and technical documentation

Comments explain mechanisms, invariants, data flow, algorithms, memory layout, or non-obvious decisions that cannot be made clear by the code itself. Keep comments to one mechanistic sentence about memory layout, data flow, algorithm, or a verifiable invariant.

Do not use comments to excuse a broken invariant, justify an unfinished feature, disguise a workaround, or claim that a requirement is met when it is not. Comments do not grant permission to violate the architecture or this guide.

Avoid narrative comments that merely recount development history, review conversations, or the author's intentions. Put change rationale in commit messages and design rationale in the relevant architecture or design documentation. An `Invariants:` block is appropriate for hard, verifiable machine-level constraints.

## Prose style

**Sentences.** Short and declarative. Policy stated flat, no softening. Hedges only where uncertainty is real, and a hedge says exactly what is known. Concrete words over adjectives. Qualifiers used literally or not at all.

**Subtraction test.** Delete the word. If nothing is lost, it never belonged. This applies to intensifiers, adjectives, signposts, and whole sentences.

**Positive statements.** State what a thing is and does. Do not define a thing by what it is not, does not, or never does in place of stating what it is and does. A sentence that exists to show the reader a forbidden thing was avoided is a compliance receipt, not documentation. If a sentence adds no useful information about the implementation, delete it, whether it is a negation, an intensifier, or a reassurance.

**One statement per fact, at the place the reader needs it.** No summary-itis: the same fact in the introduction, again in the body, and once more in a closing summary is padding. If two sentences say the same thing, delete the weaker.

**No throat-clearing openers.** A document that needs its first sentence to say what it is has a title problem, not a first-sentence problem. Delete openers of the form "This document describes..." and start with the first fact.

**No signposting filler.** "In conclusion", "Overall,", "Note that", "It is important to understand", "This section will demonstrate", "As mentioned above" spend the reader's attention announcing a move instead of making it. Cut them; a sentence worth keeping is worth keeping without the frame.

**Claims ship with evidence or a label.** A claim ships with its benchmark, its test run, or its fault injection, or it gets labeled unproven. "Tests pass" means the author ran them.

**Quote and paraphrase stay visibly distinct.** Quoted words are quoted. A paraphrase presented closely enough to the wording that a reader cannot tell it from a quote is a defect. Verdicts are recorded flat, in the speaker's own words, never hedged as "reported" and never softened with status nuances.

**Lists for data, prose for argument.** A list whose items are full sentences is a paragraph avoiding its shape. Data belongs in lists and tables; an argument belongs in prose.

**No em dashes.**

**No "not just X but Y".**

**Quality claims carry measurements.** An adjective that claims quality without a number is cut.

**One hedge per uncertainty.** A stack of hedges is cut to the single hedge that states what is known.

**No marketing verbs.** Utilize, leverage, and their kin are cut.

**No apologies or meta-comments in the document.** The document does not talk about itself.
