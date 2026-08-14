# bts

It's "another synthetics generator".

`bts` generates synthetic traces from a declarative shape file and writes them to [Braintrust](https://braintrust.dev). Describe what a trace looks like once, then generate as many as you want, spread over a historical time window, with per-trace variation.

## Install

Requires a Rust toolchain (edition 2024, so Rust 1.85 or newer). From a clone of this repo:

```sh
cargo install --path .
```

Verify with `bts --version`.

## Setup

Writing to Braintrust requires two environment variables:

```sh
export BRAINTRUST_API_KEY="sk-..."
export BRAINTRUST_PROJECT_ID="<project uuid>"
```

`BRAINTRUST_API_URL` can optionally override the default API endpoint. None of this is needed for `check` or `--dry-run`.

## Usage

Validate a shape file:

```sh
bts check syntax shape.bt        # or `bts check syntax -` to read stdin
```

Preview what would be generated, without writing anything:

```sh
bts write --from shape.bt --count 5 --over 1h --dry-run
```

Generate 200 traces spread over the last 24 hours and write them to Braintrust:

```sh
bts write --from shape.bt --count 200 --over 24h
```

Trace volume is spread linearly by default; pass `--dist sine` for a wavier load pattern. Generation is seeded — every run prints its seed, and passing `--seed <n>` reproduces a run exactly. Pass `--json` to get the final summary (seed, counts, duration, run log path) as a single JSON line on stdout, for scripts and agents.

Every `write` run also writes a JSON-lines log to `.bt/bts/logs/` (next to the nearest `.bt` project directory, or the current directory) with phase timings, the seed, per-batch insert results, retry warnings, and any failure. The last 20 runs are kept, the directory gitignores itself, and a failed run prints the path to its log. Pass `--profile` to also stream phase timings to stderr. Transient write failures (timeouts, 429s, 5xx) are retried with exponential backoff before the run gives up.

Inspect past runs without digging through the JSONL yourself:

```sh
bts check logs                   # list recent runs, each with an ok / failed verdict
bts check logs --last            # render the most recent run, one readable line per event
bts check logs <run-file-name>   # render a specific run from the listing
```

## Configuration

`bts init` scaffolds `.bt/bts/config.toml` in the current directory — an optional file controlling runtime behavior:

```toml
[log]
level = "info"            # run log verbosity: off, error, warn, info, debug, trace, or a tracing filter directive
keep_runs = 20            # run log files kept before the oldest are pruned

[http]
request_timeout = "30s"   # per-request timeout for Braintrust API calls
```

Everything is optional and shown here with its default. The `BTS_LOG` environment variable overrides `log.level` for a single run — `BTS_LOG=debug bts write ...` — and `off` disables the run log entirely.

Optionally, install the `bts` agent skill so Claude Code or Codex can write and debug shape files with the full language reference on hand:

```sh
bts setup skill claude    # or codex; --scope local|user|global
```

## The shape language

Shape files (`.bt`) declare the structure of a trace: its spans, their inputs and outputs, metadata, and how each generated trace should vary. A small example:

```bts
vars {
    model = "gpt-4o-mini"
}

trace "support-sessions" {
    repeat "turns" {
        count = range(1, 4)

        task "turn" {
            input = "question ${repeat.index}"
            output = "answer ${repeat.index}"

            llm "Chat Completion" {
                input = [{ role = "user", content = task.turn.input }]
                output = { role = "assistant", content = task.turn.output }
                metadata = { model = var.model, provider = "openai" }
                metrics = {
                    prompt_tokens = tokens(self.input)
                    completion_tokens = tokens(self.output)
                    tokens = self.metrics.prompt_tokens + self.metrics.completion_tokens
                }
            }
        }
    }

    maybe "escalation" {
        chance = 0.25

        task "escalation" {
            output = "escalated to tier 2"
        }
    }

    metadata = { escalated = maybe.escalation.included }
    tags = ["support"]
}
```

The building blocks:

- **Span blocks** — `trace`, `task`, `llm`, `tool`, and `function` nest to form the span tree, with fields like `input`, `output`, `metadata`, `metrics`, and `tags`.
- **Dynamic blocks** — `repeat`, `choice`, and `maybe` vary the shape of each generated trace: repeated sections, one-of alternatives, and probabilistic inclusion.
- **Expressions** — full expression language with arithmetic, comparisons, conditionals, string interpolation (`"${...}"`), arrays, objects, spreads, slices, and shared values via `vars`. A `vars` block can sit at the root or inside any block; each value is drawn once per instantiation of that block, so a sampled value stays consistent everywhere it's referenced.
- **References** — spans read each other's fields, so generated data stays coherent: block references like `task.turn_0.output` or `llm["Chat Completion"].metrics.tokens` thread one span's values into another, `self` reads the enclosing span's own fields, and `choice.<name>.chosen` / `maybe.<name>.included` expose what a dynamic block did. Slices project over repeat iterations — `...repeat.rounds[:repeat.index].llm.chat.output` replays an agent loop's history.
- **Functions** — a library of ~30 built-ins for randomness and data munging: samplers like `range`, `choice`, `weighted`, `normal`, and `poisson`, string helpers, math helpers, exact token counting with `tokens`, and id generators like `uuid` and `hex`.

The complete language reference (grammar, every block, field, and function) is generated by `bts setup skill`; the fixtures in `tests/fixtures/` are also worked examples.

## License

[GNU AGPL-3.0](LICENSE). Free to use, modify, and share — but any software built on it, including software offered as a network service, must be released under the AGPL as well.
