# BTS in a browser

The archive contains matching ES module bindings (`bts.js`), TypeScript declarations,
and `bts_bg.wasm`. Keep them together and serve via HTTP(S), with `.wasm` served as
`application/wasm`. No credentials, local config, filesystem, or publication API
is used. The only fetch is initialization of the WASM module.

## Build

From the repository root, with Rust edition 2024 support, Python 3, and a POSIX shell:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.126 --locked
./scripts/build-wasm.sh
```

This uses Cargo.lock and emits `target/wasm/bts-v<VERSION>-wasm.tar.gz` plus a SHA-256
checksum. The version comes from Cargo.toml, also used by `version()`. No npm or
GitHub release is published by the build script. Release maintainers can attach
these files to the corresponding BTS release. The dedicated WASM workflow builds
and tests the same archive on PRs and version tags, leaving native cargo-dist
artifacts unchanged.

## TypeScript

Copy the extracted package to your application's `vendor/bts` directory:

```ts
import init, { version, languageReference, check, generate } from './vendor/bts/bts.js';
import type { GenerationOptions, BtsError } from './vendor/bts/bts.js';

await init(); // resolves bts_bg.wasm relative to bts.js; accepts a module_or_path override
console.log(version());
const referenceForLlm = languageReference();
const source = 'trace "hello" { input = "Hello ${trace.index}" output = uuid() }';
const diagnostics = check(source); // [] means valid
const options: GenerationOptions = {
  count: 5, seed: '42',
  start: '2026-09-01T00:00:00Z', end: '2026-09-01T01:00:00Z',
  distribution: 'linear', // or sine
};
try {
  const batch = generate(source, options); // { events: [...] }, native dry-run structure
  console.log(batch.events);
} catch (error) {
  console.error((error as BtsError).message, (error as BtsError).diagnostics);
}
```

`check` runs lexing, parsing, and semantic validation. `generate` also reports
expression-evaluation diagnostics. Locations are half-open UTF-8 byte ranges with
one-based Unicode scalar line/column positions (not JavaScript UTF-16 indexes).
Option/materialization errors have an empty diagnostics array. Unknown options,
zero/nonintegral counts, malformed/overflowing seeds, and invalid windows throw.
Count is a positive u32; seed is a decimal u64 **string** to avoid JS rounding.
Start and end must include offsets and be between 1970 and 9999 UTC.

## Web Worker

Generation and token encoder initialization are synchronous and may be expensive.
Run them in a module worker to keep the UI responsive. For example, `worker.ts`:

```ts
import init, { generate } from './vendor/bts/bts.js';
import type { GenerationOptions } from './vendor/bts/bts.js';
const ready = init();
self.onmessage = async ({ data }: MessageEvent<{ source: string; options: GenerationOptions }>) => {
  try {
    await ready;
    self.postMessage({ result: generate(data.source, data.options) });
  } catch (error) {
    self.postMessage({ error });
  }
};
```

Create it with `new Worker(new URL('./worker.ts', import.meta.url), { type: 'module' })`
in a TypeScript bundler, then `worker.postMessage({ source, options })`. Plain
browser applications can use the same code as JavaScript after removing types.
Terminate the worker to cancel a large run. Callers should bound input size/count;
this interface is not a resource sandbox, and memory exhaustion can still trap.

## Compatibility

The shared parser, planner, materializer, and spec renderer define behavior. The
reference embeds the BTS release version and language/schema versions; CLI help
is omitted. Feed that reference to the caller's LLM, then validate its source.

Explicit timestamps avoid host clock and timezone dependencies. Seeded expression
values and structure match 64-bit native BTS: Xoshiro256++ explicitly fixes the
algorithm previously selected by SmallRng on the supported native release targets.
BTS still assigns fresh UUID v4 event/span IDs using browser crypto, just as native
dry-run does; normalize these IDs when comparing runs. `uuid()` expressions remain
seeded. Floating-point transcendentals can differ in the last bits across engines;
parity tests use a tight numerical tolerance. Guarantees apply to the same BTS
version and options, not across dependency or language upgrades. JSON integers
outside JavaScript's safe range have the same precision limits as JSON-parsed
native output.

File attachments are rejected because native dry-run resolves paths and assigns
upload keys using the local filesystem. Other generation makes no network calls.
No check or generate function accesses the host clock, environment, or config.

## Browser parity test

```sh
npm ci --prefix web
npx --prefix web playwright install chromium
cargo build --locked
./scripts/build-wasm.sh
node web/smoke.mjs
npm --prefix web run typecheck
```

The smoke test serves and loads real JS/WASM in Chromium, compares representative
fixture output against native dry-run, and exercises diagnostics, invalid inputs,
repeatability, version/reference consistency, and a module worker.
