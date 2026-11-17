# bts

A framework for turning graph definitions into behavior models.

Define a graph in a `.bt` shape file using `nodes`, `edges`, `forks`, and `juncts`. Kinds and properties give those elements meaning, while fields and expressions describe their data. Together, they can model relationships, data flow, alternative paths, repeated steps, and how results come together.

The language compiles these definitions into a resolved graph. Consumers interpret that graph as a behavior model using the kinds and capabilities they support. A model can describe a single expected path or many possible behaviors, with different consumers producing different results from the same definition.

Use the CLI for authoring and generation workflows, embed the Rust libraries in your own tools, or use the WebAssembly API in a browser application. The language, generation APIs, and provider integrations can work together or be used separately for the parts you need.

## How it fits together

| Surface | What it provides |
| --- | --- |
| [Language](crates/lang/) | Graph syntax, expressions, references, kind definitions, and validation. Compile shapes into a resolved graph model without running them or connecting to a service. |
| [Core APIs](crates/core/) | Mock data generation and code generation, with separate steps for evaluating behavior, formatting results, planning files, and delivering output. |
| [Providers](crates/core/src/providers/) | Bundles of kind definitions and supported capabilities that connect graphs to a particular system's data, code, and workflows. |
| [CLI](crates/cli/) | Commands to validate shapes, preview data, build code, synchronize resources, and generate authoring skills. |
| [WebAssembly API](#browser-wasm) | Validation, generation, code builds, and trace delivery for applications that supply their own UI, storage, and transport. |

Providers bundle the definitions and capabilities needed for a particular use. Their kind definitions participate in compilation, and their integration code interprets the resulting graph for supported workflows. The Braintrust provider supplies kinds such as `agent`, `span`, `tool`, `llm`, and `scorer`, along with trace formatting, generated-code integration, and workflows for datasets, experiments, and automations. These build on the language's graph primitives, keeping domain-specific contracts and behavior in the provider.

With the Braintrust provider, an agent's behavior model can produce synthetic traces or callable mock code. Dataset and scorer definitions can stand on their own or join an agent in an evaluation workflow. You can validate the graph, preview generated data locally, build code, or deliver results as separate operations. The examples below use this provider to show how these pieces fit together.

## Install

Prebuilt binaries for macOS and Linux (arm64 and x86_64) install to `~/.local/bin`:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/zdhrc/bts/releases/latest/download/bts-installer.sh | sh
```

Verify with `bts --version`. Later, `bts update` fetches and installs the latest release in place. To build from source, run `cargo install --path crates/cli` from the repository root. Installer-managed updates are not available for source builds.

## Teach your coding agent the language

Install the generated skill before asking a language model to write `.bt` files:

```sh
bts setup skill codex --scope local
# or
bts setup skill claude --scope local
```

The skill combines the language reference, Braintrust kind definitions, authoring guidance, and CLI usage. It gives your coding agent the syntax and rules for the installed version, instead of relying on guesses from similar languages. Regenerate it after updating `bts`.

Start with a concrete request, for example:

> Use the bts skill to model a support agent that looks up an invoice, answers routine questions, and escalates missing records. Keep the lookup and response consistent. Save it as support.bt, validate it, and preview a small seeded batch without writing to Braintrust.

Describe the inputs, expected outcomes, alternative paths, and relationships that matter. Ask the agent to inspect the generated payload as well as validate the source. Compilation checks the graph and its definitions, but sampled data still needs review for coherent conversations, correct parentage, and meaningful variation.

```sh
bts check syntax support.bt
bts write traces --from support.bt --count 5 --over 1h --seed 42 --dry-run
```

Neither command requires a project or API key. Start small, inspect the output, then broaden the scenarios. The [examples](examples/) provide complete shapes for conversations, tool loops, parallel work, datasets, and evaluation.

## Read a shape

This graph models a support agent that looks up an article and uses it in a response. Braintrust kinds describe the agent and its steps, while `edges` define progression, span nesting, and data flow. Save it as `support.bt`:

```bts
(support:agent) {
    input: {question: "Where can I download my invoices?"},
    output: reply.output,
};

(support)-[:next]->(request:task) {
    input: support.input,
    output: reply.output,
    tags: [|"billing"|],
};

(request)-[:calls]->(search:tool) {
    input: {query: request.input.question},
    output: {article: "Download invoices from Billing > Invoices."},
}
    -[:next, :feeds {
        value: {question: request.input.question, article: source.output.article},
        into: target.input,
    }]->(reply:llm) {
        input: <-,
        output: self.input.article,
        duration: range(0.4, 0.8),
    };
```

`(search:tool)` declares a named `node`. `(search)` refers to it. The body holds fields, which can contain literals, expressions, and references to other `nodes`. Arrays use `[|...|]`. Commas separate fields and array items, and semicolons end root statements.

`Edges` state different relationships explicitly:

| Relationship | Purpose in this shape |
| --- | --- |
| `next` | Enters the first step, or advances to a step on the same level |
| `calls` | Advances like `next` and goes down a level, so the target is a child span of its source |
| `feeds` | Supplies a value to a field marked `<-` |

The Braintrust provider interprets the `agent` `node` and every step its entry leads to as a behavior model. During trace generation, the agent does not emit a span itself. Its entry task becomes the trace root. Here `search` is a child of `request` because a `calls` edge reaches it, and `reply` follows `search` along `next`, so it is a child of `request` too.

`tool`, `llm`, and `task` are Braintrust kinds built on the language's `node` primitive. In mock generation they describe data and spans, not real tool or model calls. The `reply` reads the lookup's actual article, keeping the response consistent with the evidence in the trace.

### Branch and reconcile

`Forks` make possible paths visible in the graph. This standalone shape models alternative routes. When evaluated, it chooses one route and collects its result:

```bts
(intake:agent) {input: "Find my patient record", output: finish.output};
(intake)-[:next]->(intake_request:task) {
    input: intake.input,
    output: finish.output,
};
(intake_request)-[:calls]->(lookup:tool) {
    input: intake_request.input,
    output: {found: chance(0.8)},
};

(lookup)
    =[:exclusive]=>[
        -[known:next {when: source.output.found}]->(schedule:task) {
            output: "Route to scheduling",
        },
        -[unknown:next {when: !source.output.found}]->(register:task) {
            output: "Route to registration",
        },
    ]
        >[:first {observe: "result"} [known], [unknown]]>>(finish:task) {
            output: context.arrivals[0].value,
        };
```

`exclusive` chooses a qualifying branch. `first` observes a selected branch's result and supplies it to `finish`. `Node` names identify definitions, while named `edges` such as `[known]` identify the `junct`'s inputs. Indentation attaches continuations to the correct `fork`.

Other kinds express other behaviors: `parallel` splits work across branches, `weighted` samples alternatives, `repeat` creates a count of occurrences, and `each` distributes array items. `Juncts` such as `accumulate` and `reduce` combine results. Explicit cycle `edges` use `~[:next]~>` and can carry guards and traversal limits.

See [parallel workers](examples/fanout_parallel_workers.bt), [tool loops](examples/agent_tool_loop.bt), [multi-turn conversations](examples/multi_turn_conversation.bt), and [supervisor delegation](examples/supervisor_and_subagents.bt). The generated skill documents the available kinds, their properties, and runtime constraints.

### Extend a kind

A definition starts with `+` and is written like a `node`: its header holds property descriptors, its body holds field descriptors, and an `extends` `edge` names its base. Kinds can add schemas to existing primitives or provider definitions. For example, append this to `support.bt`:

```bts
+(billing_lookup:node {service: {type: "string", required: true}})-[:extends]->(tool);
```

Then change `(search:tool)` to `(search:billing_lookup {service: "billing"})`. Properties inside the `node` header configure that instance and are checked against the inherited schema. The body remains its data and expressions. This extension adds a contract, not a service implementation.

A field descriptor can also carry `value`, an expression that computes the field for every instance that omits it:

```bts
+(clean_query:node) {output: {type: "string", value: lower(trim(self.input))}}
    -[:extends]->(tool);
```

The expression reads `self` and `context` only, so the kind works in any shape. An instance that writes the field replaces the computed value. When both are objects they are merged by key, and the instance's keys win.

The provider's `llm` kind uses this to count tokens. Every `llm` step gets `prompt_tokens`, `completion_tokens` and `tokens` in its `metrics` without writing them, and `metrics: {time_to_first_token: 0.2}` adds to those counts. See [computed fields](examples/computed_fields.bt).

The world provider builds the setting an agent works in. A `walk` stands beside the agent, and the steps under it say how one instance is put together. A `sentence` is a walk whose parts add up to one clause, with a fork wherever there is more than one way to say it. A `list` has one item for each step, such as the policies an agent works under. The agent reads them by name, a walk emits no spans, and the same seed builds the same world in generated data and in a generated agent. See [flight change](examples/flight_change.bt).

A world holds more than sentences. A `fact` is one value and an `entity` has attributes, so a tool result and the reply that quotes it come from the same place, and a `relation` joins entities that belong together. A walk can be a step of another walk, a `persona` says its sentences its own way, and a world reads the agent's input, so the world a caller meets depends on who is calling. A `conversation` is several traces in a row that share an identifier, of a fixed or a varying number of turns. See [windowed session](examples/windowed_session.bt) and [customer support](examples/customer_support_agent.bt). A generated agent that reads a world takes `seed` and `trace`, and with those of a data run it is that trace.

The provider also writes what people say as graph on the agent's path. A `statement` is one sentence written as its grammar, and an `intent` is something a speaker wants, worded differently in each trace. Both hold values and emit no spans, so they sit before the steps that read them.

## Use the model

The following workflows consume the support graph through the Braintrust provider. Generate data from its possible behaviors, build a mock implementation, or add definitions for evaluation and resource management.

### Generate traces

Preview locally before writing a population:

```sh
bts write traces --from support.bt --count 20 --over 1h --seed 42 --dry-run
```

For live writes, install the Braintrust `bt` CLI separately, select a project with `bt switch`, and set `BRAINTRUST_API_KEY`. The selected project is read from `.bt/config.json`. `BRAINTRUST_API_URL` can override the default API endpoint.

```sh
bts write traces --from support.bt --count 200 --over 24h
```

Use `--rate 20/h` instead of `--count` for a traffic rate, or `--offset 1d` to move the window into the past. `--seed` reproduces sampled values, and `--json` makes the run summary machine-readable.

### Build a mock agent

Build the same shape into callable Python or TypeScript:

```sh
bts build agent --from support.bt --lang python --out generated/python
bts build agent --from support.bt --lang typescript --out generated/typescript
```

Builds use `uvx` for pinned Ruff formatting or `npx` for pinned Prettier formatting. The first uncached build needs network access. Generated Python requires Python 3.10 or newer, and generated TypeScript requires `Node`.js 24.2 or newer.

For Python:

```sh
cd generated/python
uv sync
uv run python -c 'from agents.support_agent import run; print(run({"question": "Where are my invoices?"}))'
```

Call `run(input)` to supply an input, or `run()` for the authored fallback. Python also exports `run_details` and `run_traced`, and TypeScript exports `runDetails` and `runTraced`, to inspect spans or attach a Braintrust parent span. The mock executes authored behavior without contacting LLMs or tools. Agent-only shapes require no project or API key to build or run.

Authored durations appear in detailed results without delaying execution. Traced spans use actual execution time. Generated agents accept an injectable random source but do not reproduce native `--seed` sampling. Local attachments remain unsupported in generated agents.

### Add cases and scorers

Append these definitions to `support.bt`:

```bts
(invoice_case:case) {
    input: {question: "Where can I download my invoices?"},
    expected: "Download invoices from Billing > Invoices.",
    tags: [|"billing"|],
};

(regressions:dataset) {cases: [|(invoice_case)|]};
(correctness:scorer) {score: context.output == context.expected ? 1 : 0};

(support_check:experiment) {
    dataset: (regressions),
    task: (request),
    scorers: [|(correctness)|],
};
```

The experiment supplies each case's input to the task and scores its result. Scorers read `context.input`, `context.output`, `context.expected`, and `context.metadata`. A scorer can also define a `judge` object with a model, prompt, and scored options, as shown in [judge_scorer.bt](examples/judge_scorer.bt).

Cases can use inline values or capture a graph `node` through `source: (search)`. See [dataset cases](examples/dataset_cases.bt) for whole traces, individual spans, and field references. [experiments.bt](examples/experiments.bt) compares basic and improved account recovery against the same five cases, linking the improved experiment with `baseline: ("recovery-baseline")`.

```sh
bts write experiments --from support.bt --dry-run
bts sync datasets --from support.bt
bts build scorers --from support.bt --lang python
```

Dataset sync reconciles rows, including removing cases no longer defined locally. Scorer builds print the `bt functions push` command and directory to run it from. After syncing the dataset and deploying scorers, run `bts write experiments --from support.bt` to evaluate generated results with the deployed functions.

Agent builds also emit local eval entrypoints when experiments are present. Select a project with `bt switch` before building them. From the generated Python directory, run:

```sh
uv run bt eval --runner python src/evals/support_check_eval.py --no-send-logs
```

`--no-send-logs` prevents uploading results. Code scorers run locally, while judge scorers still call models and incur inference costs. Eval datasets are sampled at build time with `--seed` (default `42`). Rebuild after changing cases or scorers. The [customer-support example](examples/customer_support_agent.bt) shows a larger agent with dataset cases and multiple scorers.

### Apply automations

Scorer automations select spans for deployed scoring functions. Topics automations connect facet prompts to conversation analysis. Both are graph `node` definitions managed by the Braintrust provider.

```sh
bts sync automation scorers --from examples/scoring_automations.bt --dry-run
bts sync automation topics --from examples/topics_automations.bt --dry-run
```

These previews require Braintrust access to compare against existing resources. Omit `--dry-run` to apply the changes. Deploy scorer functions before syncing scorer automations, and sync Topics automations before generating their trace population.

## Handy commands

```sh
bts check syntax support.bt
bts check perf support.bt --count 100 --over 1h
bts check logs --last
bts setup skill codex --scope local
bts update
```

CLI resource selectors, such as `--select 'agent.support'`, use command-line traversal syntax. Within a shape file, use graph references such as `(support)`.

## Browser (wasm)

The browser package validates shapes, generates Braintrust trace payloads, builds scorer and agent codebases, and writes traces through a host-supplied transport. Download `bts-wasm-web.tar.gz` from the [release assets](https://github.com/zdhrc/bts/releases), or build locally with `wasm-pack` 0.15.0 installed:

```sh
rustup target add wasm32-unknown-unknown
wasm-pack build crates/wasm --target web --out-dir ../../target/wasm-pkg -- --locked
```

The package includes JavaScript, wasm, and TypeScript declarations. Keep the JavaScript and `.wasm` files together and serve them over HTTP. This TypeScript example places the package in `bts-wasm/` beside the application module and initializes it once before calling its exports:

```ts
import init, { generate, validate, type GenerateOptions } from "./bts-wasm/bts_wasm.js";

await init();

const source = `
(support:agent) {input: "Help with billing", output: request.output};
(support)-[:next]->(request:task) {
    input: support.input,
    output: choice("resolved", "escalated"),
};
`;
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

Diagnostic ranges are UTF-8 byte offsets, not JavaScript string indexes. A diagnostic with a `source` name refers to that provider definition source rather than the user shape. Generation and build failures return `{ ok: false, error }` with a stage, message, and diagnostics. Generated JavaScript numeric values must fit the safe integer range. `spec()` returns the language reference, and `skill()` returns authoring guidance for an agent hosted by your application. For shapes containing scorers, `build(source, { project: "My Project", lang: "typescript", files: {} })` returns planned files for the host to save. Generate an agent codebase with:

```ts
import { build } from "./bts-wasm/bts_wasm.js";

const result = build(source, {
  target: "agent",
  lang: "typescript",
  project: "My Project",
  select: ["agent.support"],
  seed: "42",
  files: {},
});
```

The browser uses the same agent selection and dataset snapshots as the CLI. `project` is optional when the selected agents have no evals. The optional `files` map contains existing file contents keyed by relative path. A successful plan returns `files` and `removals`. Apply a removal only if the current file content still equals its `expected` value. Generated Python and TypeScript run in their respective runtimes. Browser generation does not invoke native formatters or execute agents.

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

Everything is optional and shown here with its default. The `BTS_LOG` environment variable overrides `log.level` for a single run, `BTS_LOG=debug bts write ...`, and `off` disables the run log entirely.

## License

[GNU AGPL-3.0](LICENSE). Free to use, modify, and share, but any software built on it, including software offered as a network service, must be released under the AGPL as well.
