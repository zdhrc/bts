use crate::providers::Provider;
use bts_lang::spec::{FuncDesc, RuleDesc, Spec as DslSpec};
use std::fmt::Write as _;
const EXAMPLES_PREAMBLE: &str = "These shapes are not hard and fast rules; they are examples to provoke your imagination. \
Steal the structure, replace the content, and expand along the axis each one names.";

/// A selectable section of a generated skill.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Part {
    /// The provider's authoring workflow.
    Workflow,
    /// How the provider presents what a shape produces.
    Presentation,
    /// The language reference, followed by the provider's kinds.
    Language,
}

pub const FULL: &[Part] = &[Part::Workflow, Part::Presentation, Part::Language];

/// Generates a standalone skill for one provider without interface commands or file access.
pub fn render<P: Provider>(spec: &DslSpec, provider: &P) -> String {
    format!(
        "---\nname: bts\ndescription: Write, review, explain, and debug bts language source.\n---\n\n{}",
        assemble(spec, provider, FULL, None, None)
    )
}

/// Assembles a provider's guidance, the language reference, and optional interface instructions.
pub fn assemble<P: Provider>(
    spec: &DslSpec,
    provider: &P,
    parts: &[Part],
    interface_preamble: Option<&str>,
    interface_reference: Option<&str>,
) -> String {
    let guidance = provider.guidance();
    let mut output = String::new();
    if let Some(interface_preamble) = interface_preamble {
        output.push_str(interface_preamble);
    }
    for part in parts {
        match part {
            Part::Workflow => output.push_str(guidance.workflow),
            Part::Presentation => output.push_str(guidance.presentation),
            Part::Language => {
                output.push_str(&reference(spec));
                output.push_str(guidance.definitions);
                let schema = provider.schema();
                for definition in schema.definitions {
                    output.push_str(&fenced_code("bts", definition.text.trim()));
                }
                if !schema.functions.is_empty() {
                    writeln!(output, "\n## {} functions\n", schema.name).unwrap();
                    writeln!(
                        output,
                        "The provider contributes these functions. Call them like the functions of the language.\n"
                    )
                    .unwrap();
                    for function in schema.functions {
                        render_function(&mut output, &function.doc);
                    }
                }
            }
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
            render_function(&mut output, function);
        }
    }

    writeln!(output, "## Graph primitives\n").unwrap();
    for primitive in spec.primitives {
        writeln!(output, "### {} ({})\n", inline_code(primitive.name), primitive.construct).unwrap();
        writeln!(output, "{}\n", primitive.summary).unwrap();
        for (label, keys) in [("Provides", primitive.provides), ("Requires", primitive.requires)] {
            if !keys.is_empty() {
                let listed: Vec<_> = keys
                    .iter()
                    .map(|key| format!("{} ({})", inline_code(&format!("context.{}", key.name)), key.ty))
                    .collect();
                writeln!(output, "{label} context: {}\n", listed.join(", ")).unwrap();
            }
        }
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

fn render_function(output: &mut String, function: &FuncDesc) {
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
    render_rules(output, function.rules);
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
    use crate::providers::braintrust::Bundle as Braintrust;
    use bts_lang::spec::SPEC as DSL_SPEC;
    #[test]
    fn renders_the_language_spec_without_interface_dependencies() {
        let skill = render(&DSL_SPEC, &Braintrust);

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
        for primitive in DSL_SPEC.primitives {
            assert!(skill.contains(primitive.name));
            assert!(skill.contains(primitive.summary));
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
        let workflow = assemble(&DSL_SPEC, &Braintrust, &[Part::Workflow], None, None);
        assert!(workflow.contains("### 1. Choose the viewer's question"));
        assert!(!workflow.contains("## Braintrust trace presentation"));
        assert!(!workflow.contains("## Surface grammar"));

        let presentation = assemble(&DSL_SPEC, &Braintrust, &[Part::Presentation], None, None);
        assert!(presentation.contains("## Braintrust trace presentation"));
        assert!(!presentation.contains("## Authoring realistic Braintrust data"));

        let language = assemble(&DSL_SPEC, &Braintrust, &[Part::Language], None, None);
        assert!(language.contains("## Surface grammar"));
        assert!(!language.contains("## Authoring realistic Braintrust data"));
        assert!(!language.contains("## Braintrust trace presentation"));
    }
}
