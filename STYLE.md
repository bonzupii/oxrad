# Style guide

## Code style

**Types first.** Bad states are unrepresentable: enums for exclusive states, newtypes for validated values, typestate for protocol ordering. Runtime checks live only at real boundaries: parsing, IO, FFI. A defensive check for an invariant the type already guarantees is a bodge, not care.

**Safe Rust.** Edition 2024. New unsafe needs explicit approval first, with the location, the measured gain, the safety argument, and the cost of the safe alternative. Approved unsafe lives in minimal leaf functions containing no safe logic; all safe logic sits in safe callers. Every unsafe site carries its safety argument, or it does not ship. Miri runs on unsafe-adjacent tests where feasible.

**Ownership.** Resolution order: shared borrow, mutable borrow, move, shared ownership only with explicit permission. A clone added to satisfy the borrow checker means the ownership model is wrong; restructure until the clone disappears or its necessity is documented and load-bearing. `Arc<Mutex<T>>` and `Rc<RefCell<T>>` are not default architecture.

**Errors.** Library code carries typed `thiserror` enums, and variants carry the invalid value as a field wherever possible. `anyhow` appears at the UI boundary only. `Box<dyn Error>` on a public API, a single `Msg(String)` variant, and `.map_err(|e| e.to_string())` are all erasure: callers cannot match on, recover from, or enumerate the failure modes. Every error reaches the user as a human-readable message; errors written only to logs are invisible failures.

**No panics on reachable paths.** No `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!`, or `unreachable!` in production code, and no indexing or slicing on computed indices without a proven bound. An `expect` with a proof is legitimate: the message states the invariant ("static regex; compiles at author time"), not a hope. Tests may fail loudly.

**No silent fallbacks.** No `unwrap_or_default`, no `unwrap_or(0)` / `unwrap_or("")`, no `let _ =` on fallible operations, no catch-all arms that swallow a case. A fallback that exists names the condition it handles, is visible to the caller, and is pinned by a test. If the caller cannot tell from outside whether the real path or the fallback ran, the fallback is the defect.

**No abstraction theater.** Generality must be exercised, not anticipated. One implementor: inline it or use an enum. Closed sets dispatch through `match` on an enum, never through `dyn Trait`. No builder where a struct literal does the job. No generic parameters instantiated at a single type. Delete the abstraction; if exactly one call site changes shape, it was theater.

**One computation per value.** Every value with more than one consumer has exactly one computation site. Duplicated derivations and copy-pasted bodies are fixed by extraction, never by syncing the copies. The coarser variant is the subsystem bypass: a feature built beside a canonical capability (its own storage, its own IO, its own error surface) instead of through it. The bypass is deleted and the feature is routed through the canonical capability; where the capability lacks something, the capability is extended, never forked.

**Small units, factored along change axes.** No god modules, structs, or functions. Each new behavior gets a unit whose reason to change is that behavior. The accretion test: if the last five features all landed in the same unit for different reasons, that unit gets factored. Thresholds: functions past 100 lines, 7+ arguments, cognitive complexity past 15 are factoring tasks, not `#[allow]` targets.

**Effects visible in types.** No hidden mutation, IO, time, randomness, or global state inside code that presents as pure computation. `static` is for compiled-once values and atomics, each named at the site. Interior mutability is for genuinely single-threaded shared state with the sharing reason stated. Everything else: thread the state through parameters, return it, or put it in a struct the caller can see. A static that exists to avoid passing a parameter is the defect wearing a convenience costume.

**Strings only at system boundaries.** File contents, user input, display text, and serialized forms are text. Internal values get types at birth: paths are `PathBuf` / `&Path`, fixed key sets are enums, modes and commands are enums the dispatcher matches on, identifiers are newtypes. A `String` field holding a mode is the defect; a `String` field holding a message is text. Parse once, carry the type.

**Performance.** Borrows over allocations in hot paths. `&str` for borrowed data, `Cow` for conditional ownership, iterators end to end without materializing. Measure the hot path, remove the allocation, measure again. Eager work in lazy positions and O(n) where the structure offers O(1) are defects, found by profiling first.

**No dead weight.** A stub is implemented, deleted, or documented as unimplemented in user-facing docs and unreachable from the UI. "Exists" is never evidence of "works": exercise the binary and confirm the code path does the thing its name promises. Dead code and dead public API kept "for reference" are deleted.

**No unnecessary complexity.** A layer with no client is deleted: no indirection stacks that forward without deciding, no states no producer reaches, no configuration surfaces for values nothing reads. The operative test is subtraction on a scratch tree: delete the layer; if behavior, tests, and readability all survive, the layer was the defect. Complexity that earns its place names its client in a mechanistic comment: the second implementor, the measured hot path, the invariant it maintains. An unnamed client is presumed absent.

**Comments.** One sentence, mechanistic, about the machine: memory layout, data flow, algorithm, or a verifiable invariant. No narrative, no retrospectives ("used to", "previously", "found in review"), no apologies, no session or phase tags, no philosophy, no fixture storytelling. Prose in comments primes the next reviewer with the author's framing instead of facts; the bug's biography belongs in the commit message's WHY section, not in the comment. An `Invariants:` block is for hard, verifiable machine-level constraints.

**Structure and naming.** One module per concern, named files, no `mod.rs`. `main.rs` holds argument parsing and app construction only. Imports run `std`, then external crates, then internal modules, one blank line between groups. Types are named after what they are: no `Manager`, `Service`, `Controller`, `Factory`, `Provider`, `Helper`, `Util`, or `Handler` in type names, no Hungarian notation, no `_impl` suffix. Newtypes carry a validated constructor returning `Result`.

**Tests.** Tests return `Result` and propagate with `?`. Assertions state the expected value, the actual value, and the tolerance. Error-path tests match the specific variant and the value it carries; `result.is_err()` alone proves nothing. A test that asserts incorrect behavior is worse than a vacuous test: it defends the bug, since the correct fix turns the suite red. Establish correct behavior from the spec first, then fix code and test as a pair and record both halves. No exemption lists, skip filters, or `#[ignore]`d tests parking known failures: the suite asserts unconditionally, and a failure is fixed or the expectation is corrected with evidence. Sabotage-check: break the code deliberately; if the test does not notice, the test is hollow. Round-trip tests never substitute for absolute reference value tests; both are required.

**Lints are a gate, not an audit.** No `#[allow]`. A green lint run says the mechanical surface is clean; it says nothing about wrong-spec tests, unexercised generality, subsystem bypasses, safety arguments, or narrative comments. Those need reading.

**Floating point.** Float arithmetic is not denied. Every float computation with a known answer carries absolute reference-value tests within a stated tolerance. No `==` or `!=` on computed floats. The NaN `RangeFolded` sentinel is detected with `is_nan()` only. Non-finite values are rejected or counted at decode boundaries, except the designed sentinel the `RangeFolded` mapping itself produces. Domain clamps where rounded inputs can leave a function's domain are stated and tested.

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

**APIs from the dependency, not memory.** Every API and idiom is checked against the version in use. A call that does not exist in that version does not ship.

**Assertions check values, not presence.** A test that passes merely because the code runs, or that asserts a thing exists without asserting what it does, proves nothing.

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
