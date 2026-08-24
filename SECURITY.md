# Security policy

## Reporting a vulnerability

Report it privately through GitHub's **Security → Report a vulnerability**
form on this repository. If that is unavailable, open an issue that says a
security report is waiting and asks for a contact address — without the details.

Please include what you did, what happened, and what you expected. A working
reproduction is worth more than a description.

There is no bounty. Reports are answered as quickly as a small project can
manage, and fixes are credited unless you would rather they were not.

## What counts

The design rests on a short list of invariants. Anything that breaks one of them
is a vulnerability, not a feature request:

- **Reaching code execution.** Any path where network input becomes Python
  source, a `bpy` expression, an attribute path, an operator name, or a shell
  command.
- **Escaping a managed root.** Any way to read or write outside the workspace
  roots through a tool argument — traversal, symlink, absolute path, UNC path,
  drive-relative path on Windows.
- **Leaking a credential.** A provider token appearing in a log line, an error
  message, a tool result, or an outbound request to anything but that provider's
  own API.
- **Reaching the local network.** Any way to make the download layer fetch a
  loopback, private, link-local or metadata-service address.
- **Escaping the operation set.** Any way to invoke a handler that is not
  registered by an explicit decorator, or to reach one under a name the registry
  does not contain.
- **Unbounded work.** Any way to make one request consume unbounded memory,
  time, or disk.

## What does not count

- **The `.blend` file is trusted.** Opening one honours Blender's own
  auto-execute settings. This server neither changes them nor adds a way to.
- **The MCP client is trusted.** Anything that can call these tools can modify
  the scene, read the managed roots and download assets. That is the job.
- **Blender bugs are Blender bugs.** Operations go through `bpy`; report those
  upstream.
- **The asset tools reach the network on purpose.** `asset.search`,
  `asset.download` and `asset.import` make outbound HTTPS requests to Poly Haven
  and Sketchfab. That is the feature, not a leak. What *would* be a
  vulnerability is a request going anywhere those providers did not name, a
  credential travelling to a CDN rather than the provider's own API, or a
  redirect escaping the URL policy — all three are listed above. Downloads can
  be turned off entirely with `BLENDER_MCP_ALLOW_ASSET_DOWNLOADS=0`; nothing
  else in the server opens a socket to anywhere but loopback.

## Known limits, stated rather than hidden

- **`scene.batch_rename` compiles a caller-supplied regular expression.** The
  pattern is capped at 200 characters and compiled by Python's `re`, which has
  no backtracking guard. A deliberately pathological pattern can therefore make
  one rename call burn CPU on Blender's main thread until the server's request
  timeout fires. It cannot execute anything, read anything or escape anywhere;
  it is a self-inflicted stall by a trusted client, which is why it is a
  documented limit and not a finding.
- **A batch occupies the main thread for its duration.** A coalesced run
  executes in one pump pass, so Blender's UI does not repaint until the run
  finishes. Batches are capped at 200 operations, and that is the bound.

## Supported versions

The project is pre-1.0. Fixes land on `main`, and there are no maintained
release branches yet.

## Details

[docs/SECURITY.md](docs/SECURITY.md) describes the execution boundary, the path
policy, the download policy and what enforces each of them.
