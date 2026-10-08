pub(crate) struct Spec {
    pub(crate) summary: &'static str,
    pub(crate) commands: &'static [CommandDesc],
}

pub(crate) struct CommandDesc {
    pub(crate) path: &'static [&'static str],
    pub(crate) guidance: &'static [&'static str],
    pub(crate) examples: &'static [&'static str],
}

pub(crate) static SPEC: Spec = Spec {
    summary: "Use the `bts` CLI to validate shapes, generate traces, build scorer functions, and sync Braintrust resources.",
    commands: &[
        CommandDesc {
            path: &["check", "syntax"],
            guidance: &["Validate a shape after changing it. Pass `-` to read source from stdin."],
            examples: &["bts check syntax shape.bt"],
        },
        CommandDesc {
            path: &["write"],
            guidance: &[
                "Choose one volume form (`--count` or `--rate`) and one window form (`--over`, optionally with `--offset`, or `--start` with `--end`).",
                "Use `--dry-run` to inspect the Braintrust event payload before writing. Pass `--seed` to reproduce sampled values.",
                "`--filter` selects blocks participating in generation by `block.kind` or `block.name`; excluding a block also excludes its descendants. Top-level scorer definitions are never write candidates. Use `--filter 'block.kind != \"scorer\"'` to omit synthetic scorer spans while retaining the shape's scorer definitions.",
                "For a live write, select the Braintrust project with `bt switch` and set `BRAINTRUST_API_KEY`. The selected project name and ID come from `.bt/config.json`.",
            ],
            examples: &[
                "bts write --from shape.bt --count 100 --over 1h --dry-run",
                "bts write --from shape.bt --count 100 --over 1h --filter 'block.kind != \"scorer\"'",
            ],
        },
        CommandDesc {
            path: &["build"],
            guidance: &[
                "Build top-level scorer blocks into SDK source files. Then change into the emitted `<out>/src/scorers` directory and run the `bt functions push --if-exists replace ...` command printed by `bts build`, passing the generated filenames from that directory.",
                "For Python scorers, the directory used to run `bt functions push` determines the bundle's import path. Passing paths from a parent directory can produce an import that fails remotely when a path component contains a hyphen.",
            ],
            examples: &["bts build --from shape.bt"],
        },
        CommandDesc {
            path: &["sync", "automations"],
            guidance: &[
                "Push scorer functions before syncing automations that reference them. Use `--dry-run` to review the proposed changes.",
            ],
            examples: &["bts sync automations --from shape.bt --dry-run"],
        },
        CommandDesc {
            path: &["sync", "datasets"],
            guidance: &[
                "A dataset block may set `description` and contain named cases. Use typed module references such as `trace[\"name\"]` for sources. Sync reconciles all rows, including deleting cases absent locally, and writes changed source traces before their rows. Use `--dry-run` to review changes.",
            ],
            examples: &["bts sync datasets --from shape.bt --dry-run"],
        },
        CommandDesc {
            path: &["check", "perf"],
            guidance: &["Measure local generation and payload size without writing traces."],
            examples: &["bts check perf shape.bt --count 100 --over 1h"],
        },
        CommandDesc {
            path: &["check", "logs"],
            guidance: &["List recent run logs, or use `--last` to inspect the newest run."],
            examples: &["bts check logs --last"],
        },
        CommandDesc {
            path: &["setup", "skill"],
            guidance: &[
                "Generate and install the combined DSL and CLI skill for an agent. Existing generated skills can be replaced; unmanaged skills are preserved.",
            ],
            examples: &["bts setup skill codex"],
        },
        CommandDesc {
            path: &["init"],
            guidance: &["Initialize the local `bts` configuration without replacing an existing one."],
            examples: &["bts init"],
        },
        CommandDesc {
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
