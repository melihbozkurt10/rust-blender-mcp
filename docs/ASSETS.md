# External asset libraries

Two providers ship: **Poly Haven** (public, no account) and **Sketchfab** (needs
an API token to download). Both sit behind one trait, and the rules that matter —
transport safety, size limits, path confinement, credential handling — live in
the crate, not in each provider.

## Tools

| Tool | Kind | What it does |
| --- | --- | --- |
| `asset.providers` | READ | Which libraries are reachable and configured |
| `asset.search` | READ | Search one provider or all of them |
| `asset.get` | READ | Full detail for one asset, including variants |
| `asset.download` | EXTERNAL | Fetch into the managed downloads directory |
| `asset.import` | EXTERNAL | Fetch, then use it in the scene |

### A typical sequence

```jsonc
{"name": "asset.search", "arguments": {
  "query": "concrete", "asset_type": "TEXTURE", "provider": "polyhaven", "limit": 5}}

{"name": "asset.import", "arguments": {
  "provider": "polyhaven", "asset_id": "concrete_floor_02",
  "resolution": 2048, "build_material": true, "name": "Concrete Floor"}}
```

`asset.import` does the whole job: downloads the maps, loads each image with the
right colour space, plans a PBR graph in Rust, creates the material and builds
the graph. For an HDRI it sets the world environment instead; for a model it
imports through the normal typed importer.

`asset.download` stops after the download, and hands back paths, sizes and
SHA-256 digests, if you would rather do the rest yourself.

## Poly Haven

Public API, no credentials. HDRIs, textures and models, all published under CC0
— which is reported as licence metadata, not reduced to a flag.

- **Resolution.** Providers publish a ladder (`1k`, `2k`, `4k`, `8k`). A request
  between rungs takes the rung **above**: asking for 3000 and silently getting
  2048 is a quality regression you cannot see, while getting 4096 is only a
  larger file. Asking for more than exists gets the largest.
- **Default resolution** is 2k if offered, otherwise the largest up to 4k.
  Downloading an 8k texture set because nobody said otherwise wastes bandwidth
  and disk.
- **Texture sets** fetch a standard set of maps — diffuse, normal (GL),
  roughness, displacement, AO, metal — at **one** resolution. Mixed resolutions
  in a single material is a rendering bug waiting to happen. Ask for specific
  maps with `maps: ["Diffuse", "Rough"]`; matching is case-insensitive, and an
  unknown map name comes back with the ones that exist.
- **Models** bring their textures with them, keeping the relative paths the
  `.blend` or `.gltf` refers to, so the model opens with its maps attached.
- Search is filtered client-side, because the API has no search parameter.
  Results are sorted by title so paging over them is stable.

## Sketchfab

```bash
export BLENDER_MCP_SKETCHFAB_TOKEN=…    # from sketchfab.com/settings/password
```

Searching works without a token; downloading does not. Without one,
`asset.download` fails with `ASSET_AUTH_REQUIRED` and the name of the variable to
set — the provider is still listed, because hiding it would make that
undiagnosable.

Sketchfab hosts work under many licences, including ones that forbid commercial
use or derivatives. See **Licences** below.

The download endpoint mints a short-lived signed URL. The token buys the URL; the
URL is then fetched **without** the token, so no credential reaches the CDN.
There is a test for exactly that.

## Licences

Licence data is passed through as the provider states it:

```jsonc
{
  "id": "by-nc",
  "name": "CC Attribution-NonCommercial",
  "url": "https://creativecommons.org/licenses/by-nc/4.0/",
  "requires_attribution": true,
  "commercial_use": false
}
```

Three rules:

1. **There is no "free to use" flag.** Whether an asset may be used is a legal
   question about a specific project, not a property of a file.
2. **Unstated is absent, not false.** If a provider does not say whether
   attribution is required, the field is omitted. "The provider did not say" and
   "the provider said no" must not look the same to someone deciding whether they
   can ship something.
3. **Derived booleans only for unambiguous identifiers.** `cc0`, `by`, `by-sa`,
   `by-nd`, `by-nc`, `by-nc-sa`, `by-nc-nd` have known terms. Anything else —
   Sketchfab's store licence, for instance — is reported by identifier and label
   alone, with both booleans unset.

`asset.providers` also returns a per-provider `license_summary` in plain
language, and a notice that this server installs nothing and executes nothing it
downloads.

## Download safety

Every rule below is enforced before a byte reaches disk. The URLs come from a
provider's API rather than from a caller, which is not a reason to trust them: a
compromised or merely buggy response must not be able to turn this process into
an HTTP client for your internal network.

- **HTTPS only.** No plain HTTP, no `file:`, no `data:`, no `ftp:`. URLs with
  embedded credentials are refused.
- **No private hosts.** Loopback, RFC 1918, link-local (including the cloud
  metadata address `169.254.169.254`), unique-local, IPv4-mapped loopback,
  carrier-grade NAT, bare hostnames, `.local` and `.internal` are all refused.
- **Redirects are limited and re-checked.** Each hop goes through the same URL
  rules, so a redirect cannot reach a host the first URL could not have named.
- **Size is capped twice.** Against the declared `Content-Length`, and against
  the bytes actually received while streaming — a server can lie about the
  length or omit it. `BLENDER_MCP_MAX_DOWNLOAD_BYTES` sets the per-file limit
  (512 MiB by default); the per-asset total is eight times that.
- **Content types are checked**, by prefix: `image/`, `model/`,
  `application/zip`, `application/octet-stream`.
- **Extensions are allowlisted** to asset formats. `.py`, `.exe`, `.dll`, `.so`,
  `.sh`, `.bat` and everything else are refused outright.
- **Filenames are validated, not sanitised.** A name with a separator, a `..`, a
  colon or a leading dot is rejected — a name that needs sanitising means the
  response is not what was expected. Relative paths inside a model archive are
  allowed up to four components, each validated the same way.
- **Nothing is executed.** No downloaded file is run, and no Blender add-on is
  ever installed from a provider. Archives stay archives; `asset.import` says so
  rather than unpacking one to an arbitrary place.
- **Downloads are atomic.** Each file is written to `.part` and moved into place
  only when complete, so an interrupted download never looks like a cached one.

## Caching

```
<workspace>/downloads/<provider>/<asset_id>/<variant>/
    manifest.json
    concrete_floor_02_Diffuse_2k.jpg
    concrete_floor_02_nor_gl_2k.exr
    …
```

`manifest.json` records each file's path, size, SHA-256 and which map it holds.
A second request for the same asset and variant is served from disk, and the
response says `from_cache: true`.

A cache hit requires every file in the manifest to still exist at the recorded
size. A manifest promising a file that has since been deleted is not a hit —
reporting it as one would hand back a path that does not exist. `force: true`
re-downloads regardless.

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `BLENDER_MCP_SKETCHFAB_TOKEN` | — | Sketchfab API token |
| `BLENDER_MCP_ALLOW_ASSET_DOWNLOADS` | `1` | Set to `0` to allow searching but not downloading |
| `BLENDER_MCP_MAX_DOWNLOAD_BYTES` | `536870912` | Per-file size cap |
| `BLENDER_MCP_WORKSPACE` | platform-specific | Parent of the downloads directory |

With downloads disabled, `asset.download` and `asset.import` fail with
`PERMISSION_DENIED` naming the variable, while search and metadata keep working.

## Adding a provider

Implement `AssetProvider`:

```rust
fn id(&self) -> &'static str;
fn info(&self) -> ProviderInfo;
fn authorization(&self) -> Option<Authorization>;      // default: None
fn search(&self, query: &SearchAssets) -> …<Vec<AssetSummary>>;
fn get(&self, asset_id: &str) -> …<AssetSummary>;
fn plan(&self, request: &DownloadAsset) -> …<DownloadPlan>;
```

A provider builds URLs and parses JSON. It never fetches, never writes to disk,
and never decides what is safe — the shared `Fetcher` and `Downloader` do that,
which is why a new provider inherits every rule above for free.

`plan` returns a `DownloadPlan`: the asset, the variant chosen, and a list of
`PlannedFile`s, each with a URL, a filename, an optional map name, an optional
declared size, and whether the URL needs credentials. Mark that last flag `false`
for pre-signed CDN URLs — the downloader only sends the token where the plan says
to.

Testing needs no network: `StubFetcher` answers from a map of URL to canned JSON
or bytes, and records which URLs received an `Authorization` header.
