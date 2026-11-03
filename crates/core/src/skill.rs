use bts_lang::spec::{Cardinality, NamePolicy, Place, RuleDesc, Spec as DslSpec};
use std::fmt::Write as _;
const EXAMPLES_PREAMBLE: &str = "These shapes are not hard and fast rules; they are examples to provoke your imagination. \
Steal the structure, replace the content, and expand along the axis each one names.";

/// A selectable section of the generated Braintrust skill.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Part {
    BraintrustWorkflow,
    BraintrustPresentation,
    Language,
}

pub const FULL_BRAINTRUST: &[Part] = &[Part::BraintrustWorkflow, Part::BraintrustPresentation, Part::Language];

/// Generates a standalone Braintrust skill without interface commands or file access.
pub fn render(spec: &DslSpec) -> String {
    format!(
        "---\nname: bts\ndescription: Write, review, explain, and debug bts language source.\n---\n\n{}",
        assemble_braintrust(spec, FULL_BRAINTRUST, None, None)
    )
}

/// Assembles Braintrust guidance, the language reference, and optional interface instructions.
pub fn assemble_braintrust(
    spec: &DslSpec,
    parts: &[Part],
    interface_preamble: Option<&str>,
    interface_reference: Option<&str>,
) -> String {
    let mut output = String::new();
    if let Some(interface_preamble) = interface_preamble {
        output.push_str(interface_preamble);
    }
    for part in parts {
        match part {
            Part::BraintrustWorkflow => output.push_str(crate::providers::braintrust::skill::WORKFLOW),
            Part::BraintrustPresentation => output.push_str(crate::providers::braintrust::skill::PRESENTATION),
            Part::Language => output.push_str(&reference(spec)),
        }
    }
    if let Some(interface_reference) = interface_reference {
        output.push_str(interface_reference);
    }
    output
}

/// Renders the language guidance for inclusion in an interface's own skill.
pub fn reference(spec: &DslSpec) -> String {
    let mut output = String::new();
    writeln!(output, "# `{}` language\n", spec.name).unwrap();
    writeln!(output, "{}\n", spec.summary).unwrap();
    writeln!(
        output,
        "Treat this generated reference as authoritative for `bts` language version {} (specification schema version {}).\n",
        spec.language_version, spec.schema_version
    )
    .unwrap();

    writeln!(output, "## Surface grammar\n").unwrap();
    writeln!(output, "Notation: {}\n", spec.surface.notation).unwrap();
    writeln!(output, "{}", fenced_code("ebnf", spec.surface.grammar.trim())).unwrap();

    if !spec.surface.notes.is_empty() {
        writeln!(output, "### Syntax notes\n").unwrap();
        for note in spec.surface.notes {
            writeln!(output, "- {note}").unwrap();
        }
        writeln!(output).unwrap();
    }

    writeln!(output, "## Expression forms\n").unwrap();
    for expression in spec.expressions {
        writeln!(output, "### {}\n", inline_code(expression.id.as_str())).unwrap();
        writeln!(output, "{}\n", expression.summary).unwrap();
        writeln!(output, "Syntax: {}\n", inline_code(expression.syntax)).unwrap();
        if !expression.examples.is_empty() {
            writeln!(output, "Examples:").unwrap();
            for example in expression.examples {
                writeln!(output, "- {}", inline_code(example)).unwrap();
            }
            writeln!(output).unwrap();
        }
        render_rules(&mut output, expression.rules);
    }

    if !spec.functions.is_empty() {
        writeln!(output, "## Functions\n").unwrap();
        for function in spec.functions {
            writeln!(output, "### {}\n", inline_code(function.name)).unwrap();
            writeln!(output, "{}\n", function.summary).unwrap();
            writeln!(output, "Syntax: {}\n", inline_code(function.syntax)).unwrap();
            if !function.examples.is_empty() {
                writeln!(output, "Examples:").unwrap();
                for example in function.examples {
                    writeln!(output, "- {}", inline_code(example)).unwrap();
                }
                writeln!(output).unwrap();
            }
            render_rules(&mut output, function.rules);
        }
    }

    writeln!(output, "## Blocks\n").unwrap();
    for block in spec.blocks {
        writeln!(output, "### {}\n", inline_code(block.keyword)).unwrap();
        writeln!(output, "{}\n", block.summary).unwrap();
        writeln!(output, "- Syntax: {}", inline_code(block.syntax)).unwrap();
        writeln!(output, "- Name: {}", name_requirement(block.name)).unwrap();
        writeln!(output, "- Allowed in: {}", placements(spec, block.allowed_in)).unwrap();
        writeln!(
            output,
            "- Unlisted fields: {}\n",
            if block.body.open { "allowed" } else { "rejected" }
        )
        .unwrap();

        if !block.body.fields.is_empty() {
            writeln!(output, "| Field | Value | Cardinality | Description |").unwrap();
            writeln!(output, "| --- | --- | --- | --- |").unwrap();
            for field in block.body.fields {
                writeln!(
                    output,
                    "| {} | {} | {} | {} |",
                    inline_code(field.keyword),
                    markdown_cell(&inline_code(&field.value.to_string())),
                    cardinality(field.cardinality),
                    markdown_cell(field.summary),
                )
                .unwrap();
            }
            writeln!(output).unwrap();
        }

        if !block.conventions.is_empty() {
            writeln!(output, "Conventions:").unwrap();
            for convention in block.conventions {
                writeln!(output, "- {convention}").unwrap();
            }
            writeln!(output).unwrap();
        }

        render_rules(&mut output, block.rules);
    }

    writeln!(output, "## Global rules\n").unwrap();
    if spec.rules.is_empty() {
        writeln!(output, "No additional global validation rules are defined.\n").unwrap();
    } else {
        render_rules(&mut output, spec.rules);
    }

    if !spec.examples.is_empty() {
        writeln!(output, "## Complete examples\n").unwrap();
        writeln!(output, "{EXAMPLES_PREAMBLE}\n").unwrap();
        for example in spec.examples {
            let validity = if example.valid { "Valid" } else { "Invalid" };
            writeln!(output, "### {}: {}\n", validity, example.summary).unwrap();
            if !example.note.is_empty() {
                writeln!(output, "{}\n", example.note).unwrap();
            }
            writeln!(output, "{}", fenced_code("bts", example.source.trim())).unwrap();
        }
    }

    output
}

fn render_rules(output: &mut String, rules: &[RuleDesc]) {
    if rules.is_empty() {
        return;
    }

    writeln!(output, "Rules:").unwrap();
    for rule in rules {
        writeln!(output, "- {}: {}", inline_code(rule.id.as_str()), rule.summary).unwrap();
    }
    writeln!(output).unwrap();
}

fn name_requirement(name: NamePolicy) -> &'static str {
    match name {
        NamePolicy::Forbidden => "forbidden",
        NamePolicy::Optional => "optional",
        NamePolicy::Required => "required",
    }
}

fn cardinality(cardinality: Cardinality) -> &'static str {
    match cardinality {
        Cardinality::Optional => "optional",
        Cardinality::Required => "required",
        Cardinality::Repeated => "repeated",
    }
}

fn placements(spec: &DslSpec, allowed_in: &[Place]) -> String {
    allowed_in
        .iter()
        .map(|place| match *place {
            Place::Root => "document root".to_owned(),
            Place::Block { id } => spec
                .block_by_id(id)
                .map(|block| inline_code(block.keyword))
                .unwrap_or_else(|| inline_code(id.as_str())),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn markdown_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}

fn inline_code(value: &str) -> String {
    let longest_run = value
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or_default();
    let fence = "`".repeat(longest_run + 1);

    format!("{fence}{value}{fence}")
}

fn fenced_code(language: &str, value: &str) -> String {
    let longest_run = value
        .lines()
        .flat_map(|line| line.split(|character| character != '`'))
        .map(str::len)
        .max()
        .unwrap_or_default();
    let fence = "`".repeat((longest_run + 1).max(3));

    format!("{fence}{language}\n{value}\n{fence}\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use bts_lang::spec::SPEC as DSL_SPEC;
    #[test]
    fn renders_the_language_spec_without_interface_dependencies() {
        let skill = render(&DSL_SPEC);

        assert!(skill.starts_with("---\nname: bts\ndescription:"));
        assert!(skill.contains(DSL_SPEC.summary));
        assert!(skill.contains(DSL_SPEC.surface.grammar.trim()));
        for expression in DSL_SPEC.expressions {
            assert!(skill.contains(expression.id.as_str()));
            assert!(skill.contains(expression.syntax));
            assert!(skill.contains(expression.summary));
        }
        for function in DSL_SPEC.functions {
            assert!(skill.contains(function.name));
            assert!(skill.contains(function.syntax));
            assert!(skill.contains(function.summary));
        }
        for block in DSL_SPEC.blocks {
            assert!(skill.contains(block.keyword));
            assert!(skill.contains(block.syntax));
            assert!(skill.contains(block.summary));
            for field in block.body.fields {
                assert!(skill.contains(field.keyword));
                assert!(skill.contains(field.summary));
            }
            for convention in block.conventions {
                assert!(skill.contains(convention));
            }
        }
        assert!(skill.contains(crate::providers::braintrust::skill::WORKFLOW));
        assert!(skill.contains(crate::providers::braintrust::skill::PRESENTATION));
        assert!(!reference(&DSL_SPEC).contains("## Authoring realistic Braintrust data"));
        assert!(skill.contains(EXAMPLES_PREAMBLE));
        for example in DSL_SPEC.examples {
            assert!(skill.contains(example.summary));
            assert!(skill.contains(example.note));
            assert!(skill.contains(example.source.trim()));
        }
        for rule in DSL_SPEC.rules {
            assert!(skill.contains(rule.id.as_str()));
            assert!(skill.contains(rule.summary));
        }
        assert!(!skill.contains("## CLI commands"));
        assert!(!skill.contains("bts check syntax"));
        assert!(!skill.contains("--offset"));
    }

    #[test]
    fn skill_parts_can_be_selected_independently() {
        let workflow = assemble_braintrust(&DSL_SPEC, &[Part::BraintrustWorkflow], None, None);
        assert!(workflow.contains("### 1. Choose the viewer's question"));
        assert!(!workflow.contains("## Braintrust trace presentation"));
        assert!(!workflow.contains("## Surface grammar"));

        let presentation = assemble_braintrust(&DSL_SPEC, &[Part::BraintrustPresentation], None, None);
        assert!(presentation.contains("## Braintrust trace presentation"));
        assert!(!presentation.contains("## Authoring realistic Braintrust data"));

        let language = assemble_braintrust(&DSL_SPEC, &[Part::Language], None, None);
        assert!(language.contains("## Surface grammar"));
        assert!(!language.contains("## Authoring realistic Braintrust data"));
        assert!(!language.contains("## Braintrust trace presentation"));
    }
}
