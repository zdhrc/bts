# bts

It's "another synthetics generator".

`bts` creates synthetic workflows in [Braintrust](https://braintrust.dev): traces, runnable mock agents, datasets, scorer-backed experiments, and Topics automations. Describe a workflow in a shape file, then generate varied traffic over a historical time window, build regression cases, and compare versions with real scorer results.

## Install

Prebuilt binaries for macOS and Linux (arm64 and x86_64) install to `~/.local/bin`:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/zdhrc/bts/releases/latest/download/bts-installer.sh | sh
```

Verify with `bts --version`. Later, `bts update` fetches and installs the latest release in place.

To build from source instead, `cargo install --path crates/cli` from a clone of this repo (edition 2024, so Rust 1.85 or newer). Source builds have no install receipt, so `bts update` only works for installer-managed installs.

## Quickstart

Initialize the directory you'll generate traces from:

```sh
bts init
```

This scaffolds `.bt/bts/config.toml`, an optional file controlling runtime behavior (see [Configuration](#configuration)). The `.bt` directory is a shared home for Braintrust tooling; everything `bts` owns — config, run logs — lives under its own `.bt/bts/` corner.

Next, install the `bts` agent skill so Claude Code or Codex can write and debug shape files with the full language reference on hand:

```sh
bts setup skill claude    # or codex; --scope local|user|global
```

Select a Braintrust project with `bt switch`. Both scorer builds and live writes use
the selected project's name and ID from `.bt/config.json`. Writing also requires an API key:

```sh
export BRAINTRUST_API_KEY="sk-..."
```

`BRAINTRUST_API_URL` can optionally override the default API endpoint. The project context
and API key are not needed for `check` or `--dry-run`.

Then describe a trace in a shape file (see [The shape language](#the-shape-language)) and write a batch:

```sh
bts write traces --from shape.bt --count 200 --over 24h
```

## The shape language

A shape file (`.bt`) describes a workflow and the data around it. Save this small example as `shape.bt`:

```bts
vars { answer = "Download invoices from Billing > Invoices." }

trace "support" {
    input = { question = "Where can I download my invoices?" }
    output = llm.reply.output
    tags = ["billing"]

    tool "search" {
        input = trace.input.question
        output = var.answer
    }
    llm "reply" {
        input = { question = trace.input.question, article = tool.search.output }
        output = tool.search.output
        duration = range(0.4, 1.2)
    }
}

scorer "answer-correctness" {
    code { score = output == expected ? 1 : 0 }
}

dataset "support-cases" {
    case "invoices" {
        input = { question = "Where can I download my invoices?" }
        expected = var.answer
    }
}

facet "Request type" {
    prompt = "Describe the customer's request in a short phrase."
}
automation "support-topics" {
    type = "topics"
    facets = ["Request type"]
    scope = "trace"
}

experiment "support-check" {
    dataset = dataset["support-cases"]
    task = trace.support
    scorers = [scorer["answer-correctness"]]
}
```

The trace's inputs and outputs are synthetic. Scorers and Topics run in Braintrust against that data. Here's what you can do with each part.

### Traces

Nest `task`, `llm`, `tool`, and `function` spans to show a workflow. References connect their inputs and outputs; expressions vary durations, token counts, metadata, and content. Use `repeat`, `choice`, and `maybe` to vary the structure too:

```bts
trace "support-session" {
    repeat "turns" {
        count = range(1, 4)
        task "turn" {
            input = "question ${repeat.index}"
            output = "answer ${repeat.index}"
        }
    }
    maybe "escalation" {
        chance = 0.25
        task "handoff" { output = "Escalated to tier 2." }
    }
}
```

```sh
bts write traces --from shape.bt --count 200 --over 24h
```

See [multi-turn conversations](examples/multi_turn_conversation.bt), [agent tool loops](examples/agent_tool_loop.bt), and [supervisors and subagents](examples/supervisor_and_subagents.bt) for richer workflows.

### Scorers

Code scorers evaluate `input`, `output`, `expected`, and `metadata`. You can also use an LLM judge:

```bts
scorer "helpfulness" {
    judge {
        model = "gpt-4o-mini"
        prompt = "Rate the response to this request. Request: ${input}. Response: ${output}."
        options = { helpful = 1, unhelpful = 0 }
    }
}
```

Code builds require `uvx` (installed with `uv`) for Python or `npx` (installed with npm) for TypeScript. Both scorer and agent builds format generated source with pinned Ruff or Prettier versions. The first uncached build needs network access to download the formatter.

Build scorers into Python or TypeScript SDK source files:

```sh
bts build --from shape.bt --lang typescript
```

The build prints the `bt functions push` command and directory to run it from. Once deployed, functions can score experiments or incoming traces. Add an automation to apply the judge to replies:

```bts
automation "review-replies" {
    type = "scorer"
    scorers = ["helpfulness"]
    scope = "span"
    span_names = ["reply"]
}
```

```sh
bts sync automation scorers --from shape.bt
```

See [code scorers](examples/code_scorer.bt), [LLM judges](examples/judge_scorer.bt), and [online scoring](examples/scoring_automations.bt).

### Agents

An `agent` declares a callable mock whose behavior comes from a whole trace:

```bts
agent "support" { entrypoint = trace.support }
trace "support" {
    input = { question = "Where are my invoices?" }
    output = "You asked: ${self.input.question}"
}
```

`bts build agent --from shape.bt --lang python --out generated` emits modules under `src/agents`. Call `run(input)` to supply an input or `run()` to use the authored input. Python also exports `run_details` and `run_traced`, and TypeScript exports `runDetails` and `runTraced`, for inspecting spans or attaching a Braintrust parent span. These agents execute authored mock behavior without making LLM or tool calls.

The [customer-support example](examples/customer_support_agent.bt) includes an agent, inline dataset cases, and code scorers. Select a project with `bt switch` before building its eval entrypoint. A shape without experiments does not require a project or API key to build or run its agent.

For Python 3.10 or newer:

```sh
bts build agent --from examples/customer_support_agent.bt --lang python --out generated/python
(
    cd generated/python
    uv sync
    uv run python -c 'from agents.customer_support_agent import run; print(run())'
    uv run bt eval --runner python src/evals/customer_support_eval.py --no-send-logs
)
```

For Node.js 24.2 or newer:

```sh
bts build agent --from examples/customer_support_agent.bt --lang typescript --out generated/typescript
(
    cd generated/typescript
    npm install
    npm run build
    node --input-type=module -e 'import {run} from "./src/agents/customer_support_agent.mts"; console.log(run())'
    bt eval --runner node src/evals/customer_support.eval.ts --no-send-logs
)
```

Install `bt` separately for these eval commands. `--no-send-logs` evaluates the local cases without uploading results. Omit it to create a Braintrust experiment with your configured credentials, or use `--dev` for the eval development server. Generated evals pass only the case input to the agent and keep expected answers available to scorers. Import agent or scorer implementation modules when you only need their functions. Eval entrypoints register during CLI discovery and execute during a normal import. Import `evals/<name>_definition.py` or `evals/<name>.definition.ts` to inspect cases or call `evaluate` without starting a run.

Choose an agent or experiment with repeatable `--select` flags. Selecting an agent includes its evals. Selecting an experiment includes its agent, dataset, scorers, and baseline chain. Omit selection to build every agent and experiment:

```sh
bts build agent --from shape.bt --select 'agent.support' --lang python --out generated
bts build agent --from shape.bt --select 'experiment.improved' --seed 42 --lang typescript --out generated
```

Generated eval datasets support dynamic inline expressions, trace sources, span sources, and ordered groups. Cases are sampled at build time with `--seed` (default `42`), so rebuilding with the same source and seed reproduces the cases. Trace and span sources supply their input and use their output as expected unless the case overrides it. Groups become ordered arrays of inputs and expected outputs. Repeated references within a case share the same sampled instance. Case metadata and tags are preserved.

Both code and judge scorers run locally through the Braintrust SDK. Judge evals use the authored prompt, model, and choice scores through `autoevals`, without a separate scorer deployment. Running judge scorers requires model credentials and incurs inference costs even with `--no-send-logs`. Rebuild with `bts build agent` after changing a local judge. `bts build scorers` generates its deployment registration.

Eval builds also emit an ordered suite runner: `uv run python src/evals/run_evals.py` or `node src/evals/run-evals.mts`. It runs baselines first and links candidates to the experiment IDs returned by those runs. Add `--no-send-logs` to execute the suite locally without uploading comparisons. Individual `bt eval` entrypoints remain available and resolve an authored baseline by its existing experiment name.

Agent entrypoints must be whole traces. Local attachments and nested scorer spans remain unsupported in generated agents. Use the existing trace and experiment commands for those shapes.

Generated agents accept an injectable random source, but do not reproduce `bts write traces --seed` sampling. Authored durations appear in the detailed results without delaying execution. Traced spans use actual execution time.

### Datasets

Author cases with inputs, expected results, metadata, and tags, or capture a whole trace or selected span as a case:

```bts
dataset "search-cases" {
    case "invoice-lookup" {
        span = trace.support.tool.search
        tags = ["billing"]
    }
}
```

```sh
bts sync datasets --from shape.bt
```

Sync keeps Braintrust datasets aligned with the cases in your file as you add, change, or remove them. See [dataset cases](examples/dataset_cases.bt) for the different ways to create rows.

### Topics

Facets describe the signals you want to discover. Topics automations use them to group conversations in Braintrust, such as billing requests or reasons customers might leave:

```sh
bts sync automation topics --from shape.bt
```

Sync the automation before writing traces. See [Topics automations](examples/topics_automations.bt) for conversations covering renewal costs, unresolved issues, and customers who change their minds after a fix.

### Experiments

An experiment connects a dataset, a trace or span to generate, and deployed scorers. Each row supplies the task's input; the task generates a synthetic result and the scorers produce its scores.

After syncing the dataset and deploying the scorers:

```sh
bts write experiments --from shape.bt --select 'experiment["support-check"]'
```

Repeat `--select` to choose several experiments, or omit it to write all. Add `baseline = experiment["baseline-name"]` to compare against another experiment in the same write. `--dry-run` previews local cases and generated tasks without invoking scorers.

See [the account recovery comparison](examples/experiments.bt) for baseline and improved flows, five cases, and three scorers.

The full language reference is generated by `bts setup skill`; [examples/](examples/) has complete workflows to start from.

## Handy commands

```sh
bts check syntax shape.bt                                    # validate a shape file (`-` reads stdin)
bts write traces --from shape.bt --count 5 --over 1h --dry-run # preview traces
bts write experiments --from shape.bt --dry-run               # preview experiment tasks
bts check logs --last                                        # inspect the most recent run
bts update                                                  # update to the latest release
```

Use `--rate 20/h` instead of `--count` for a traffic rate, and `--offset 1d` to move a trace window into the past. `--seed` reproduces sampled values; it doesn't control deployed scorer randomness. Live writes print a summary; `--json` makes it machine-readable. Run logs live under `.bt/bts/logs/`.

## Browser (wasm)

The browser package validates shapes, generates Braintrust trace payloads, builds scorer files, and writes traces through a host-supplied transport. For releases that include browser support, download `bts-wasm-web.tar.gz` from the [release assets](https://github.com/zdhrc/bts/releases). To build an unreleased checkout locally with `wasm-pack` 0.15.0 installed:

```sh
rustup target add wasm32-unknown-unknown
wasm-pack build crates/wasm --target web --out-dir ../../target/wasm-pkg -- --locked
```

The package includes JavaScript, wasm, and TypeScript declarations. Keep the JavaScript and `.wasm` files together and serve them over HTTP. This TypeScript example places the package in `bts-wasm/` beside the application module and initializes it once before calling its exports:

```ts
import init, { generate, validate, type GenerateOptions } from "./bts-wasm/bts_wasm.js";

await init();

const source = 'trace "support" { output = choice("resolved", "escalated") }';
const validation = validate(source);
if (!validation.valid) throw new Error(JSON.stringify(validation.diagnostics));

const generationOptions: GenerateOptions = {
    count: 5,
    over_ms: 60_000,
    until_ms: 1_735_689_600_000,
    seed: "42",
    id_prefix: "support-batch-1",
};
const preview = generate(source, generationOptions);
if (!preview.ok) throw new Error(`${preview.error.stage}: ${preview.error.message}`);
console.log(preview.value.payload.events);
```

Supply the seed as a decimal `u64` string, either `count` or `rate_per_second`, and either `over_ms` or `start_ms`. Time options are integer milliseconds: `start_ms` and `until_ms` are epoch timestamps, while `over_ms` and optional `offset_ms` are durations. `until_ms` defaults to the current time. Repeating a payload requires an explicit time window and `id_prefix` as well as the seed. Use a fresh prefix for each independent write, and reuse it only when intentionally retrying the same batch.

Diagnostic ranges are UTF-8 byte offsets into the source, not JavaScript string indexes. Generation and build failures return `{ ok: false, error }` with a stage, message, and diagnostics. Generated JavaScript numeric values must fit the safe integer range. `spec()` returns the language reference, and `skill()` returns authoring guidance for an agent hosted by your application. For shapes containing scorers, `build(source, { project: "My Project", lang: "typescript", files: {} })` returns planned files for the host to save, using optional existing file contents keyed by relative path.

For delivery, the host owns authentication and supplies an asynchronous transport. This example assumes your application implements `/api/braintrust/*`: the backend authenticates the caller, authorizes the project, adds server-side Braintrust credentials, and forwards the response status, headers, and body. API keys stay out of static browser assets.

```ts
import { write, type HttpTransport } from "./bts-wasm/bts_wasm.js";

const transport: HttpTransport = async request => {
    try {
        const response = await fetch(`/api/braintrust${request.path}`, {
            method: request.method,
            headers: request.headers,
            body: new Uint8Array(request.body),
            signal: request.signal,
        });
        return {
            status: response.status,
            headers: Object.fromEntries(response.headers.entries()),
            body: new Uint8Array(await response.arrayBuffer()),
        };
    } catch (error) {
        throw {
            message: "Braintrust transport failed",
            retryable: error instanceof TypeError
                || (error instanceof DOMException && error.name === "AbortError"),
        };
    }
};

const result = await write(source, generationOptions, { project_id: "YOUR_PROJECT_UUID" }, transport);
if (!result.ok) {
    console.error(result.error.stage, result.error.message, result.error.details);
} else {
    console.log(result.value.row_ids);
}
```

Send the supplied `Uint8Array` body once, honor the abort signal, and consume the response body before resolving. Core handles chunking and retries, including `Retry-After`. A transport rejection can set `retryable: true` for a connection failure or timeout. Write failures report acknowledged row IDs in `error.details`, and interrupted requests may have written additional rows. Browser writes currently reject attachments before sending requests. Generation still returns attachment descriptors for the host to resolve. Dataset, automation, and experiment synchronization are not exposed by the browser package.

## Configuration

`.bt/config.json` is the shared `bt` project context. Its `project` and `project_id`
select the project for generated scorers and live writes.

`.bt/bts/config.toml` (scaffolded by `bts init`) controls runtime behavior:

```toml
[log]
level = "info"            # run log verbosity: off, error, warn, info, debug, trace, or a tracing filter directive
keep_runs = 20            # run log files kept before the oldest are pruned

[http]
request_timeout = "30s"   # per-request timeout for Braintrust API calls
```

Everything is optional and shown here with its default. The `BTS_LOG` environment variable overrides `log.level` for a single run — `BTS_LOG=debug bts write ...` — and `off` disables the run log entirely.

## License

[GNU AGPL-3.0](LICENSE). Free to use, modify, and share — but any software built on it, including software offered as a network service, must be released under the AGPL as well.
