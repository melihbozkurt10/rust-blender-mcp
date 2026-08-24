# Testing

Most of this system can be tested without Blender, and is. What genuinely needs
Blender is tested inside Blender, headless, with no GPU.

| Suite | Needs | Time | What it covers |
| --- | --- | --- | --- |
| `cargo test --workspace --all-features` | Rust | seconds | Protocol, framing, domain maths, workflows, cache, providers, tool schemas |
| `crates/mcp-server/tests/protocol_parity.rs` | Rust | seconds | Rust ↔ Python agreement, and the no-execution rule |
| `tests/protocol/test_error_parity.py` | Python | instant | The error taxonomy on both sides |
| `scripts/verify_repo.py` | Python | instant | Repository invariants |
| `scripts/smoke_test.py` | Blender | ~30 s | Every bridge operation, in-process |
| `tests/blender/test_bridge_roundtrip.py` | Blender + the server | ~60 s | The whole stack over a real socket |
| `tests/blender/test_standalone_smoke.py` | Blender + the server | ~30 s | The eleven steps a new user takes, on a stock Blender with no add-ons |
| `crates/asset-providers/tests/live_polyhaven.rs` | network | ~5 s | That the real Poly Haven API matches the parser |
| `tests/blender/test_asset_import.py` | network + Blender | ~90 s | Provider → download → Blender, for real |
| `tests/blender/test_scene_surface.py` | Blender | ~10 s | Surface grouping and openings against real, rotated geometry |

The last two reach the network, so neither runs by default: the Rust one is
`#[ignore]`d and the Python one is a script you run deliberately.

## The Rust suite

```bash
cargo test --workspace --all-features
```

518 tests. The interesting ones are not "does this function return a
value" but "does this refuse the thing it must refuse":

- **`blender-protocol`** — validation. A negative focal length, an inverted frame
  range, a non-finite vector, an unknown enum, a traversal in a path. Every
  rejection is checked for its *code*, not just for failing.
- **`blender-client`** — framing under adversarial conditions: a frame split
  across reads, several frames in one read, an oversized length prefix, invalid
  UTF-8, a disconnect mid-frame, a response for a request that already timed out.
- **`blender-domain`** — the maths, against known answers. The camera framing
  solve is checked against hand-computed distances; the wall builder against
  measured corners.
- **`workflow-engine`** — every workflow, against a recording executor. Each one
  is run to completion and to failure, and the compensations are checked in
  order. No Blender involved.
- **`scene-cache`** — revision folding, expiry, session changes, and that a
  diff over an expired revision returns `REVISION_EXPIRED` rather than a partial
  answer.
- **`asset-providers`** — URL policy (loopback, private ranges, metadata
  endpoint, redirects, schemes), extension and filename rules, size caps,
  provider response parsing against recorded JSON, cache hits and misses, and
  that a token never reaches a CDN.
- **`mcp-server`** — every tool has an object schema with no `$ref`, tool names
  are unique, categories activate and deactivate correctly, and the artifact
  store confines paths.

Tests are named as sentences, because a failing test name should tell you what
broke without opening the file:

```
a_diff_from_too_far_back_expires_rather_than_lying
credentials_only_go_to_urls_that_asked_for_them
the_local_network_is_out_of_reach
plain_http_is_refused
an_unrecognised_licence_stays_unstated_rather_than_permissive
```

## Parity and invariants

```bash
cargo test -p blender-mcp-server --test protocol_parity
python tests/protocol/test_error_parity.py
python scripts/verify_repo.py
```

These are the tests that keep the two languages honest:

- every forwarding tool has a handler in the add-on, and the set of tools that
  deliberately have none is declared explicitly, so a typo in a tool name fails
  the build;
- side-effect classifications agree, because a read that is really a write would
  be retried after a dropped connection and applied twice;
- no tool and no handler has a name suggesting code execution;
- the add-on's syntax tree contains no `eval`, `exec`, `compile`, `__import__`,
  `os.system` or `subprocess`;
- the error taxonomy matches, name for name and value for value.

## Inside Blender

```bash
blender --background --python scripts/smoke_test.py
```

Imports the add-on and calls every registered handler in-process — no socket, no
server. Around 480 assertions over 228 operations, in about thirty seconds. This
is the test to run after touching the add-on: it is fast, it needs nothing else
running, and it catches the `bpy` API differences that no amount of Rust testing
can.

It is also where Blender-version surprises show up. Every one of these was found
here or by the end-to-end test, not by reading release notes:

- Blender 5.1 has no `WORKBENCH` engine.
- Blender 4.4+ Actions have no `.fcurves`; they have slots, layers and
  channelbags.
- Modifiers reject ID properties, but expose `persistent_uid`.
- `object.dimensions` reads zero before the depsgraph has evaluated.
- `scene.frame_set` re-evaluates animation, overwriting a value you just wrote —
  so the frame must be set *before* the value.
- `FunctionNodeRandomValue` has four output sockets all named `Value`.

## Against the real provider

```bash
cargo test -p asset-providers --test live_polyhaven -- --ignored --nocapture
```

Canned responses prove the parser is self-consistent, not that it matches what
Poly Haven actually sends. These seven tests close that gap: the asset listing,
one asset's real resolution ladder, an HDRI plan, a texture set (one resolution,
`nor_gl` present and `nor_dx` absent), a model with its texture paths intact, and
one real 1k HDR downloaded and checked byte-wise — including that it starts with
the Radiance signature, which a redirect body or a JSON error page would not.

They also run every planned path and URL back through the download policy, so a
real listing that the policy would reject fails here rather than in front of a
user.

## Surfaces

```bash
blender --background --factory-startup --python tests/blender/test_scene_surface.py
```

Runs inside Blender against real geometry: a façade, a yard and a door, built
and then turned 37, 90 and -128 degrees. It checks that a wall comes back as one
region rather than a pile of triangles, that its area and extent are the ones it
really has, that a downward-facing plane is a ceiling and not a floor, that an
unmarked door is not an opening, and that moving the object throws the derived
surfaces away.

Rotation is the point of the fixture. Grouping faces in local space and calling
the answer world space is the kind of bug that looks right on a building that
happens to sit axis-aligned.

## The asset pipeline, end to end

```bash
python tests/blender/test_asset_import.py
```

36 checks: search, licence metadata, the security refusals, then a real HDRI
downloaded and applied as the world environment, a cache hit on the second
request, and a real texture set turned into a material whose graph is inspected
inside Blender — image nodes, normal map, AO mix, valid links, and every data map
loaded as `Non-Color`.

This test earned its keep immediately. It found two bugs that every unit test
missed:

* `resolve_managed_path` appended a trailing separator when the path already
  existed (`join("")` does that), so every `image.load` from the downloads root
  reported the file as missing.
* The PBR graph addressed `ShaderNodeMix` sockets by name. That node has two
  inputs called `Factor`, four called `A`, four called `B` and four outputs
  called `Result` — one set per data type — so any texture set with an AO map
  failed to build at all.

Both now have unit tests. Neither could have been caught without a real provider
and a real Blender at the same time.

## The whole stack

```bash
python tests/blender/test_bridge_roundtrip.py
```

It starts both halves itself: the server binary on a free port, then Blender
headless with `scripts/run_bridge.py`, which loads the add-on and dials in — the
same thing a user's Blender does. Pass `--blender` and `--binary` when they are
not where it looks.

70 checks over a real socket: handshake and capability negotiation, ids surviving
a rename, a batch with typed references, an atomic batch rolling back, a workflow
rolling back through compensating actions, events reaching the cache, `scene.diff`
reporting what changed, and an artifact produced by a real render.

`run_bridge.py` starts Blender headless with the add-on loaded and dials into the
server, which is what a user's Blender does.

## A stock Blender, from scratch

```bash
python tests/blender/test_standalone_smoke.py
```

Eleven steps over real MCP against a Blender started with `--factory-startup`,
so no third-party add-on is loaded and every `BLENDER_MCP_*` variable is
stripped from the environment first: connect, create a cube, rename it,
transform it, bevel it, author a Principled material, assign it, create an area
light, create a camera and frame it, inspect the scene, delete everything and
purge the orphans.

It exists because every other suite runs in an environment that has been set up.
This one asserts that a first-time user with nothing configured gets a working
system, and it is the suite to run first when something looks broken.

## Writing a test

**Test the refusal.** A validator that accepts good input is half-tested. The
test that matters is the one asserting the error code for bad input, and that the
details name the field.

**Name the reason.** `assert_eq!(x, y, "a material must not mix resolutions")`
turns a failure into an explanation.

**No network in tests.** `asset-providers` has `StubFetcher`, which answers from
a map of URL to canned response and records which URLs were sent credentials. A
test that reaches the internet is a test that fails on a train.

**No Blender in Rust tests.** `workflow-engine` has a recording executor. If a
piece of logic needs Blender to be tested, that is usually a sign the logic
should have been in Rust.

## Continuous integration

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-features
python scripts/verify_repo.py
```

The Blender suites need a Blender install, and two suites need the network, so
those run where they can:

```bash
blender --background --python scripts/smoke_test.py
python tests/blender/test_bridge_roundtrip.py
cargo test -p asset-providers --test live_polyhaven -- --ignored
python tests/blender/test_asset_import.py
```
