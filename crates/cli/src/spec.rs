pub(crate) struct Spec {
    pub(crate) summary: &'static str,
    pub(crate) commands: &'static [CommandDesc],
}

pub(crate) struct CommandDesc {
    #[allow(dead_code)]
    pub(crate) tags: &'static [&'static str],
    pub(crate) path: &'static [&'static str],
    pub(crate) guidance: &'static [&'static str],
    pub(crate) examples: &'static [&'static str],
}

pub(crate) static SPEC: Spec = Spec {
    summary: "Use the `bts` CLI to validate shapes, generate traces, build scorer functions and runnable agents, and sync Braintrust resources.",
    commands: &[
        CommandDesc {
            tags: &[],
            path: &["check", "syntax"],
            guidance: &["Validate syntax and semantic definitions after changing a shape. Pass `-` to read source from stdin."],
            examples: &["bts check syntax shape.bt"],
        },
        CommandDesc {
            tags: &[],
            path: &["write", "traces"],
            guidance: &[
                "Use one `bts write` per population. Use another run of the same shape with a different `--offset`, `--dist`, or seed only when the populations need separate time windows or volumes.",
                "Choose one volume form (`--count` or `--rate`) and one window form (`--over`, optionally with `--offset`, or `--start` with `--end`).",
                "Use `--dry-run` to inspect the Braintrust event payload before writing. Pass `--seed` to reproduce sampled values.",
                "`--filter` selects blocks participating in generation by `block.kind` or `block.name`; excluding a block also excludes its descendants. Top-level scorer definitions are never write candidates. Use `--filter 'block.kind != \"scorer\"'` to omit synthetic scorer spans while retaining the shape's scorer definitions.",
                "For a live write, select the Braintrust project with `bt switch` and set `BRAINTRUST_API_KEY`. The selected project name and ID come from `.bt/config.json`.",
            ],
            examples: &[
                "bts write traces --from shape.bt --count 100 --over 1h --dry-run",
                "bts write traces --from shape.bt --count 100 --over 1h --filter 'block.kind != \"scorer\"'",
            ],
        },
        CommandDesc {
            tags: &["preview"],
            path: &["write", "experiments"],
            guidance: &[
                "An experiment selects a dataset, a whole trace or static span subtree, and scorer references. Sync its dataset and build/push its scorers before a live write. Live generation consumes the synced dataset snapshot, including reviewed row changes.",
                "Each row replaces the selected root's input and expected fields. Its metadata overrides matching root keys, and tags are appended. Use self.input or the selected block's input in output expressions. The selected subtree retains its original reference scopes.",
                "--select accepts experiment traversals; repeat it to choose several. Omitting it writes all experiments. Referenced baselines must be selected in the same write and are written first. Every invocation creates new runs with a shared generated name suffix.",
                "Upload summaries retain created experiment IDs on failure. --json includes an error and per-experiment status (complete or incomplete); incomplete uploads may contain some results. Scorer failures identify the experiment, dataset row, and scorer.",
                "--dry-run previews local cases and tasks without API access or scorer invocation; uploaded dataset corrections are only consumed by live writes. Authored synthetic scorer spans are excluded. Live writes invoke deployed scorers, so judge scorers incur model costs. --seed controls generation, not scorer randomness.",
            ],
            examples: &["bts write experiments --from shape.bt --select 'experiment.baseline' --seed 42 --dry-run"],
        },
        CommandDesc {
            tags: &["preview"],
            path: &["build"],
            guidance: &[
                "The optional target is scorers (the default) or agent. Agent builds create runnable python or typescript modules under <out>/src/agents, with eval entrypoints when experiments are present.",
                "Both build targets require uvx for Python formatting or npx for TypeScript formatting. The first uncached build downloads pinned Ruff or Prettier. Generated agents require Python 3.10+ or Node.js 24.2+.",
                "Declare an agent with `agent \"support\" { entrypoint = trace.support }`. The entrypoint must be a whole trace. Call the generated run(input) function with an input, or run() for the authored input. run_details/runDetails returns span details, and run_traced/runTraced attaches execution to a parent span.",
                "For Python, run uv sync in the output directory, then uv run python to import agents.<name>_agent. For TypeScript, run npm install and npm run build, then import ./src/agents/<name>_agent.mts with Node. Agent-only shapes need no project or API key to build or run.",
                "Shapes with experiments require the project selected by bt switch. From the output directory, use `uv run bt eval --runner python src/evals/<experiment>_eval.py --no-send-logs` or `bt eval --runner node src/evals/<experiment>.eval.ts --no-send-logs`. Install bt separately. Omit --no-send-logs to upload with configured credentials, or use --dev for the eval development server.",
                "Generated eval entrypoints register during CLI discovery and execute during a normal import. The *_definition.py and *.definition.ts modules can be imported without starting a run. Import agent or scorer implementation modules for standalone functions. Eval tasks receive only case input, while expected answers stay available to scorers.",
                "Use repeatable --select agent or experiment traversals to choose a build. Agent selection includes matching evals; experiment selection includes its baseline chain and required agents, datasets, and scorers. Omitting --select builds all agents and experiments.",
                "Generated evals snapshot dynamic inline, trace, span, and grouped dataset cases at build time. --seed defaults to 42. Source input becomes case input, source output becomes expected unless overridden, and groups become ordered arrays. References within one case share the same sampled instance. Metadata and tags are preserved.",
                "Generated evals support code and judge scorers. Judges use autoevals locally without a separate deployment. Judge inference requires model credentials and incurs costs even with --no-send-logs. Rebuild agents to refresh local judges after edits; build scorers generates deployment registrations. Eval builds include src/evals/run_evals.py or src/evals/run-evals.mts for ordered execution linked by the returned experiment IDs. The suite accepts --no-send-logs for local execution without uploaded comparisons. Individual bt eval entrypoints resolve baselines by their existing experiment names.",
                "Local attachments and nested scorer spans remain unsupported in generated agents. Generated agents execute mock behavior without calling LLMs or tools.",
                "Generated agents accept an injectable random source but do not reproduce bts write traces --seed sampling. Authored durations appear in detailed results without delaying execution, while traced spans use actual execution time.",
                "Build top-level scorer blocks into SDK source files. Then change into the emitted `<out>/src/scorers` directory and run the `bt functions push --if-exists replace ...` command printed by `bts build`, passing the generated filenames from that directory.",
                "For Python scorers, the directory used to run `bt functions push` determines the bundle's import path. Passing paths from a parent directory can produce an import that fails remotely when a path component contains a hyphen.",
            ],
            examples: &[
                "bts build --from shape.bt",
                "bts build agent --from shape.bt --lang python --out generated",
            ],
        },
        CommandDesc {
            tags: &["preview"],
            path: &["sync", "automation", "scorers"],
            guidance: &[
                "Push scorer functions before syncing automations that reference them. Use `--dry-run` to review the proposed changes. --select accepts scorer or automation traversals; repeat it to select multiple resources. Matching automations are synced in full.",
            ],
            examples: &["bts sync automation scorers --from shape.bt --select 'scorer.brand' --dry-run"],
        },
        CommandDesc {
            tags: &["preview"],
            path: &["sync", "automation", "topics"],
            guidance: &[
                "Sync referenced facet definitions, their topic maps, and Topics automations. No scorer build or push is required. Use --dry-run to review without writes.",
                "Sync reconciles resources previously managed from the same source path and selected project; locally removed definitions are deleted remotely after successful updates. All sync commands share .bt/bts/state.json, grouped by project ID and shape path. Keep this state for removal and recovery tracking; content comparisons read Braintrust.",
                "Prompt changes apply to future traces by default. Add --regenerate to reprocess the existing Topics lookback window for automations whose referenced facet prompt changed. Interrupted regeneration is resumed on the next sync.",
                "--select accepts facet or automation traversals; repeat it to select multiple resources. Matching automations are synced in full, with all their dependencies. Omitting --select syncs all Topics automations.",
            ],
            examples: &["bts sync automation topics --from shape.bt --select 'facet[\"Churn risk\"]' --dry-run"],
        },
        CommandDesc {
            tags: &["preview"],
            path: &["sync", "datasets"],
            guidance: &[
                "Removing a previously synced dataset block deletes its remote dataset on the next sync. .bt/bts/state.json tracks ownership by project ID and shape path. Definitions shared by another shape are retained. An empty shape can remove all datasets previously synced from it; --select can address a previously synced dataset that is now absent locally. Dry runs do not write state.",
                "A dataset block may set `description` and contain named cases. Use typed module references such as `trace[\"name\"]` for sources. Sync reconciles all rows, including deleting cases absent locally, and writes changed source traces before their rows. Use `--dry-run` to review changes. --select accepts a dataset traversal, such as dataset.regressions or dataset[\"Regression cases\"]; repeat it to select multiple datasets. Only selected datasets and their required source traces are reconciled.",
            ],
            examples: &["bts sync datasets --from shape.bt --select 'dataset.regressions' --dry-run"],
        },
        CommandDesc {
            tags: &[],
            path: &["check", "perf"],
            guidance: &["Measure local generation and payload size without writing traces."],
            examples: &["bts check perf shape.bt --count 100 --over 1h"],
        },
        CommandDesc {
            tags: &[],
            path: &["check", "logs"],
            guidance: &["List recent run logs, or use `--last` to inspect the newest run."],
            examples: &["bts check logs --last"],
        },
        CommandDesc {
            tags: &[],
            path: &["setup", "skill"],
            guidance: &[
                "Generate and install the combined DSL and CLI skill for an agent. Existing generated skills can be replaced; unmanaged skills are preserved.",
            ],
            examples: &["bts setup skill codex"],
        },
        CommandDesc {
            tags: &[],
            path: &["init"],
            guidance: &["Initialize the local `bts` configuration without replacing an existing one."],
            examples: &["bts init"],
        },
        CommandDesc {
            tags: &[],
            path: &["update"],
            guidance: &["Update the installed `bts` binary to its latest released version."],
            examples: &["bts update"],
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::SPEC;
    use crate::cmd::Cli;
    use clap::{Command, CommandFactory as _};
    use std::collections::HashSet;

    fn leaf_paths(command: &Command, prefix: Vec<String>, found: &mut HashSet<Vec<String>>) {
        if command.get_subcommands().next().is_none() {
            found.insert(prefix);
        } else {
            for child in command.get_subcommands() {
                let mut path = prefix.clone();
                path.push(child.get_name().to_owned());
                leaf_paths(child, path, found);
            }
        }
    }

    #[test]
    fn documents_every_cli_leaf() {
        let mut actual = HashSet::new();
        leaf_paths(&Cli::command(), Vec::new(), &mut actual);
        let documented = SPEC
            .commands
            .iter()
            .map(|command| command.path.iter().map(|part| (*part).to_owned()).collect())
            .collect::<HashSet<Vec<String>>>();
        assert_eq!(documented, actual);
    }
}
