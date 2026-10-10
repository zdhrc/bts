# bts

It's "another synthetics generator".

`bts` creates synthetic workflows in [Braintrust](https://braintrust.dev): traces, datasets, scorer-backed experiments, and Topics automations. Describe a workflow in a shape file, then generate varied traffic over a historical time window, build regression cases, and compare versions with real scorer results.

## Install

Prebuilt binaries for macOS and Linux (arm64 and x86_64) install to `~/.local/bin`:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/zdhrc/bts/releases/latest/download/bts-installer.sh | sh
```

Verify with `bts --version`. Later, `bts update` fetches and installs the latest release in place.

To build from source instead, `cargo install --path .` from a clone of this repo (edition 2024, so Rust 1.85 or newer). Source builds have no install receipt, so `bts update` only works for installer-managed installs.

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
