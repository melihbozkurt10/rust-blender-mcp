# Contributing

Bug reports, new tools and new bridge operations are all welcome. The project
has a few hard rules, and everything else is negotiable.

## The rules

**No arbitrary code execution, ever.** No `execute_python`, no `run_script`, no
`eval`, no `exec`, no `compile`, no `__import__`, no `subprocess`, no
`os.system`, and no tool that takes a `bpy` expression. This is not a style
preference — it is the reason the project exists, and four separate tests fail
the build if it stops being true. A pull request that adds an escape hatch will
be declined however useful the escape hatch is.

**Everything is typed.** A tool takes a Rust struct with a derived JSON schema
and validates before anything reaches Blender. No free-form payloads, no
`serde_json::Value` parameters, no stringly-typed enums.

**Validate in Rust, not in `bpy`.** If a request can be refused without Blender,
refuse it in Rust with an error that names the field, gives the value and lists
the alternatives. Blender's main thread should never be blocked by work that was
always going to fail.

**Say what you do not know.** An operation that cannot answer must report that,
never guess. "The data is not available" and "the thing does not exist" are
different answers and must stay different.

## Adding an operation

Three files, in this order:

1. **`crates/blender-protocol/src/<domain>.rs`** — the payload struct, its
   `JsonSchema` derive, and an `impl Validate` if it has cross-field
   constraints. Add the tests for the refusals here.
2. **`blender_extension/operations/<domain>.py`** — the handler, registered with
   `@op("name")`, `@read("name")` or `@external("name")`. The name is a literal;
   the dispatch table must stay readable by reading the source. Import the
   module from `operations/__init__.py`.
3. **`crates/mcp-server/src/tools/<domain>.rs`** — a `ToolSpec::forward::<P>` if
   the tool passes its arguments straight through, or `ToolSpec::custom` if it
   does work in the server first.

Then:

```bash
cargo test -p blender-mcp-server --test protocol_parity
```

That test fails if the three sides disagree — a tool with no handler, a
side-effect class that differs between languages, or a name that looks like code
execution.

## Naming

`domain.thing.verb`. A forwarding tool has exactly the same name as its bridge
operation, so a name in a log, an error and a batch step all mean the same
thing.

Prefer one coherent operation over a family of setters: `camera.update` takes
lens, sensor, clipping and projection together rather than offering
`camera.set_lens` and five siblings.

## Keeping the numbers honest

Tool counts, category counts, bridge operation counts and error-code counts
appear in the README and in `docs/`. They are checked against the build, not
trusted:

```bash
python scripts/check_docs.py            # fails if any documented count is wrong
python scripts/check_docs.py --write    # regenerate the tables marked `generated:`
python scripts/check_docs.py --json     # the real numbers, for a script
```

CI runs the first form. If you add a tool or an operation, run the second and
commit what it changes. Anything between `<!-- generated:… -->` markers is
produced by that script; edit the source, not the table.

## Benchmarks

`benchmarks/` measures the shipped product from outside, over the same stdio MCP
transport a client uses. Nothing in the server or the add-on imports it.

```bash
cargo build --release
python scripts/benchmark.py                # everything, needs Blender
python scripts/benchmark.py --no-blender   # the suites that do not
python scripts/benchmark.py --quick        # a fast sanity check, not for publishing
```

Results land in `benchmarks/results/`. Two rules:

- **Measure before optimising, and again afterwards.** A change justified by
  "this looked slow" is not justified.
- **Do not publish a number the suite did not produce**, and label an estimate
  as an estimate. The token counts are estimates; the byte counts are not.

## Before opening a pull request

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
python scripts/verify_repo.py
python tests/protocol/test_error_parity.py
python scripts/check_docs.py
```

If the change touches the add-on, also run the in-Blender suites:

```bash
blender --background --factory-startup --python scripts/smoke_test.py
python tests/blender/test_bridge_roundtrip.py
```

If you cannot run those — no Blender, no GPU, whatever the reason — say so in the
pull request rather than leaving it implied. An untested claim is worse than an
acknowledged gap.

## Tests

Test the refusal. A validator that accepts good input is half tested; the test
that matters asserts the error *code* for bad input and that the details name
the field.

No network in Rust tests: `asset-providers` has a stub fetcher. No Blender in
Rust tests: `workflow-engine` has a recording executor. If a piece of logic
needs Blender to be tested, that is usually a sign it should have been in Rust.

## Licence

Contributions are accepted under Apache-2.0, the licence of the project.
Do not paste code from another project into this one unless its licence permits
it and the attribution comes with it.
