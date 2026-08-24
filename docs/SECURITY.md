# Security

The premise: an MCP server is a program that lets a language model, prompted by
text it did not write, drive an application on your machine. Every boundary here
is designed for that.

## The rule

**Network input never becomes code.**

There is no tool named `execute_python`, `run_python`, `eval_python`,
`execute_script`, `run_script`, `shell`, `exec` or `eval`, and there is no tool
that does what one would do under another name. There is no generic `bpy`
expression endpoint, no dynamic import, no temporary Python file written and
executed, and no subprocess spawned per operation.

This is not a promise in a document. Four things enforce it:

1. **The add-on's dispatcher is a fixed table.** An incoming `op` is a key into
   a `dict` populated at import time by decorators. A name that is not in the
   table is `UNSUPPORTED_OPERATION`. There is no fallback, no prefix matching,
   and no path that turns the name into an attribute lookup or an operator call.

2. **`scripts/verify_repo.py` parses the add-on's syntax tree** and fails if it
   finds `eval`, `exec`, `compile`, `__import__`, `os.system`, `os.popen`,
   `importlib.import_module`, or an import of `subprocess`, `runpy`, `pty`,
   `ctypes` or `multiprocessing`. Parsing rather than grepping, so a docstring
   that mentions `eval` does not trip it and a real call cannot hide behind
   formatting.

3. **`crates/mcp-server/tests/protocol_parity.rs`** walks the real tool list and
   the real handler table and fails if any name contains a word suggesting
   execution (`exec`, `eval`, `shell`, `python`, `script`, `subprocess`,
   `install`, …). It matches whole words, so `render.execute` is fine and
   `execute_python` is not.

4. **The same test proves the two halves agree** — a tool that forwards to an
   operation the add-on does not implement fails the build, which keeps the
   dispatch table the complete and only description of what can happen.

Run them:

```bash
python scripts/verify_repo.py
cargo test -p blender-mcp-server --test protocol_parity
```

The same rule covers the files this server writes. There is no tool that takes
a fragment of a file format -- XML, a byte patch, a snippet of anything -- and
applies it to an exported asset. Everything written out is generated from state
that has already been validated, so a wrong result is fixed by authoring it
correctly and exporting again rather than by editing the output.

## Attribute assignment

Setting an arbitrary attribute from a network string is the same hole wearing a
different hat, so it is not available either.

- Typed operations set typed fields. `camera.update` maps a fixed list of
  argument names to a fixed list of attributes; the mapping is a literal in the
  source.
- Where an RNA **data path** is genuinely the interface — keyframing is the
  honest example, since `keyframe_insert(data_path=…)` is Blender's own API —
  the path is validated against a character allowlist on **both** sides, and it
  is resolved through `path_resolve`, which is RNA traversal, not evaluation.
  A path containing a parenthesis, a semicolon or anything outside
  `[A-Za-z0-9_.\[\]"' -]` is rejected.
- Node sockets are set by name or identifier against the node's declared socket
  list, and an unknown one comes back with the list of the ones that exist.

## Network exposure

- The bridge listens on `127.0.0.1:9877`. Binding anywhere else requires
  `BLENDER_MCP_ALLOW_REMOTE=1`, and the server refuses to start otherwise.
- There is no authentication, because there is no remote access. If you set
  `BLENDER_MCP_ALLOW_REMOTE=1` you are exposing full scene mutation to anything
  that can reach the port. Do not.
- Frames are capped (16 MiB by default). An oversized frame closes the
  connection rather than being buffered.

## Filesystem

Every path a caller supplies is relative to a **managed root**, and the caller
picks the root by name, not by path:

| Root | Contents |
| --- | --- |
| `project` | The project directory; imports and exports default here |
| `downloads` | Assets fetched from external providers |
| `renders` | Render output and viewport screenshots |
| `exports` | Exported geometry |
| `temp` | Scratch, cleared between sessions |
| `cache` | Server-owned caches |

Resolution canonicalises the path and verifies the result is still inside the
root. `..`, absolute paths, drive letters, UNC prefixes and symlinks that escape
are all `PATH_NOT_ALLOWED`. The add-on additionally refuses any path that is not
absolute, because the *server* is what resolves paths — the add-on never turns a
relative path into a filesystem location of its own accord.

Large binaries never travel as inline base64. A render produces an **artifact
reference** — an id, a path relative to a managed root, a size and a MIME type —
and the file stays where it is.

## Credentials

- Tokens are read from the environment (`BLENDER_MCP_SKETCHFAB_TOKEN`) and
  nowhere else. There is no tool argument that accepts a credential, because a
  credential in an argument ends up in a transcript.
- A token lives in a `Secret`, whose `Debug` and `Display` both print
  `<redacted>`. A struct that derives `Debug` and happens to hold one — the
  server's whole `Config`, for instance — cannot leak it through a log line.
- A token is sent only to the provider's own API, never to a CDN. Sketchfab's
  download endpoint returns a *signed* URL; that URL is fetched with no
  `Authorization` header at all. There is a test for exactly this.
- Error messages carry the request URL with its query string stripped, because a
  signed URL's query string is itself a credential.
- Nothing is committed: `verify_repo.py` and `package_addon.py` both refuse to
  proceed if a file looks like it contains a token.

## Child processes

There are none. The server starts no program: Blender is the only thing on the
other end of the socket, and it is a process the user launched. There is no
`tokio::process`, no `std::process::Command`, no shell invocation and no path in
the code that could grow one without failing review -- the whole point of a
closed operation set is that "run this for me" is not one of the operations.

The dev-time scripts under `scripts/` and `tests/` do launch Blender, because
that is what an installer and a test harness are for. Neither is part of the
server and neither takes network input.

## Downloads

Covered in full in [ASSETS.md](ASSETS.md). The short version:

- HTTPS only. No plain HTTP, no `file:`, no `data:`.
- Loopback, private, link-local, unique-local and carrier-grade-NAT addresses are
  refused, along with bare hostnames and `.local` / `.internal` names — so a
  malformed provider response cannot turn this process into an HTTP client for
  your internal network, including the cloud metadata endpoint at
  `169.254.169.254`.
- Redirects are limited and **each hop is re-checked**, so a redirect cannot
  reach a host the original URL could not have named.
- Size is capped against the declared length *and* against the bytes actually
  received, because a server can lie about `Content-Length` or omit it.
- File extensions are allowlisted to asset formats. `.py`, `.exe`, `.dll`, `.sh`
  and friends are refused outright.
- Filenames and relative paths from a provider are validated component by
  component; anything with a separator, a `..`, a drive letter or a leading dot
  is rejected rather than sanitised.
- **Nothing downloaded is ever executed**, and no add-on is ever installed from
  a provider. Archives are left as archives.

## Licences

Licence metadata is passed through exactly as the provider states it. There is no
"free to use" boolean anywhere in the codebase, because whether an asset may be
used is a legal question about a specific project, not a property of a file.

Where a provider's terms are unstated, the field is **absent**, not `false` —
"the provider did not say" and "the provider said no" must not look the same to
someone deciding whether they can ship something. Only licence identifiers whose
terms are unambiguous (`cc0`, `by`, `by-nc`, …) get the derived booleans filled
in; anything else is reported by identifier and label alone.

## What is still trusted

Being honest about the boundary:

- **The .blend file.** Opening one runs whatever auto-execute settings Blender
  is configured with. This server does not change those settings, and does not
  add a way to.
- **The MCP client.** Anything that can call these tools can modify the scene,
  read the managed roots, write renders and download assets. The server is as
  trusted as the client driving it.
- **Blender itself.** Operations go through `bpy`; a bug in Blender is a bug in
  this system too.
- **The scene.** Surface and opening queries return grouped geometry and
  authored metadata, never `bpy` handles and never raw mesh access. What they
  report is only as true as what is in the .blend.

## Reporting

Found a way to reach code execution, escape a managed root, or leak a token?
That is a real bug, not a feature request. Open an issue with the reproduction —
or, if you would rather not publish it, whatever private channel the repository
lists.
