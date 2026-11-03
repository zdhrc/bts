//! The Braintrust demo authoring workflow.

pub const WORKFLOW: &str = r#"## Authoring realistic Braintrust data

Use this workflow for demos and exploratory Braintrust data. Aim for high-fidelity data that is nearly indistinguishable from the company's real workflow. Design backward from a useful investigation, then build forward from plausible interactions. Follow these phases for a real agent, a partly known company, or an invented use case. The evidence available and the decisions within each phase will differ. A throughput benchmark has a different goal and does not need to follow this story.

### 1. Choose the viewer's question

Choose who will inspect the data, such as a product manager, engineer, or domain expert. Decide what they should be able to learn about the agent's behavior, which evidence would let them investigate it, and what action they could take afterward. An executive may care about business risk, an engineer about the failing step, and a product manager about unmet needs. If the request is for one feature, give it one clear payoff instead of turning the demo into a tour. Pick a useful question or behavior family, not the exact answer every trace must produce.

Working result: one investigation goal and the evidence a viewer would need to pursue it.

### 2. Gather and assess context

Use the path that fits the user's request. Each path still follows the remaining phases.

- **A real company and agent:** Read the available customer brief, product documentation, repository, agent prompts, tool contracts, and representative interactions or traces. Learn what the company actually does, what the agent can do, and what it is expected to do. Use public company and industry sources when useful. Do not invent capabilities or policies that conflict with the evidence.
- **A real company with limited agent access:** Ground the product, customers, terminology, and industry constraints in available sources. Treat the unseen agent's architecture and behavior as assumptions. Keep verified facts distinct from plausible assumptions while authoring, and ask for missing details when they determine the core workflow.
- **A concept or “demo like this” request:** Use the supplied example to understand the kind of experience the user wants. Research the relevant industry if its workflows or constraints matter, then invent a coherent company, agent, tools, and customer population. Treat another company's product as inspiration unless the user asked to model it directly. A fictional world still needs specific rules and consistent behavior.

The starting material may be an empty Braintrust project, existing traces, a short discovery brief, or a full codebase. Use the most relevant context already available before asking the user to restate it. Scale research to the time and evidence available. Keep research notes and assumptions out of generated trace tags and metadata unless they are genuine fields of the modeled application.

Working result: the facts, assumptions, and constraints that the world must respect. This can stay in the agent's working context; it need not become a repository file.

### 3. Establish the world and behavior contract

Define the company and its product, the customers and personas who use it, and the agent's responsibilities. Give the agent expected behavior, available tools, and limits. Include the account state, history, policies, and other facts that affect an interaction. Each customer should have a plausible goal and way of speaking. Vary the details while keeping the world's rules consistent.

Give the world a few recurring pressures that make its work interesting, such as deadlines, incomplete records, handoffs, changing customer intent, or tool failures. Choose pressures that fit this company and industry. Show how they affect decisions instead of adding dramatic language to otherwise ordinary requests.

Define what competent behavior looks like for the agent, what counts as a minor quality issue, and what would be a consequential failure. The agent may answer, take actions, hand work to a human, or operate under a strict policy. Its actual authority, responsibilities, and limits should determine these distinctions.

Working result: a consistent company, customer population, agent role, and standard for judging its behavior.

### 4. Design a credible population

Most traces should show ordinary, competent work. Include a smaller share of modest quality problems, such as a missed detail, incomplete answer, or slightly misguided next step. Reserve rare paths for consequential failures, escalations, and unusual situations. Choose proportions that fit the workflow rather than a fixed quota. A broad demo needs everyday traffic to compare problems against; a focused demo may need a specific workflow slice. A rare but costly failure can still deserve attention.

Dream up happy and unhappy paths from the world's actual tasks, constraints, and customer goals. Include varied situations within each path, not one script repeated with different names. A useful problem may emerge from several small decisions rather than one obvious failure. Avoid making every unusual case the exact issue a facet or scorer is meant to find.

One shape generates every trace in a run. Literal content repeats unchanged. Ask what differs between trace 0 and trace 99. Vary intents and outcomes with authored alternatives, turn and step counts with `repeat`, and correlated rare paths with `maybe`. Skew volume toward common cases and give long or unusual cases a smaller share. Never use an index as customer-facing content. Use `choice` around whole alternative paths so their messages, tool calls, and outcomes agree. Use `weighted` or a long-tailed distribution when common cases should dominate.

Working result: scenario families with plausible frequencies, varied details, and causes for both ordinary and unusual outcomes.

### 5. Make the behavior observable

Decide which inputs, tool results, agent decisions, and outputs a person must see to understand each outcome. If the customer has an existing tracing scheme, follow its real execution path where possible. Otherwise, create a tree whose nesting reflects responsibility and whose data shows cause and effect. Carry facts forward with block references so later decisions agree with earlier evidence. Detailed naming, payload, and metric guidance lives in the separate Braintrust presentation reference.

Working result: a trace tree that lets a person follow the cause of each outcome from input through agent decisions to final output.

### 6. Connect discovery to improvement

Use the same world and agent behavior across traces, facets, datasets, scorers, and experiments. Each resource should help a team investigate or improve something visible in the others.

Plan a short investigation a viewer can follow: notice a behavior among ordinary traffic, inspect the supporting traces, identify a recurring or costly issue, and take a useful next step. That step may be a Topic, a regression dataset, or an experiment, depending on the request. Choose the smallest complete workflow that shows the requested value. Use only capabilities supported by the current bts and Braintrust workflows. Leave enough variation and no-match cases for the finding to feel discovered instead of predetermined.

- **Topics and facets:** Start from a question a team would ask of its traffic. Define the evidence that supports a finding, what is insufficient, how later events can change the interpretation, and what a useful summary should say. Include ordinary no-match traces. Put evidence in the interaction and let Topics surface the pattern instead of announcing it in tags or scripted phrasing.
- **Golden datasets:** Keep one task and one input/output contract per dataset. Include representative routine, boundary, and failure cases with defensible expected behavior. A golden answer should express the behavior to preserve, not merely copy the agent's generated output.
- **Judge calibration datasets:** Include strong, borderline, and poor outputs for the same rubric. Give each case a reasoned target label or score so a reviewer can tell whether the judge separates meaningful quality differences. Keep calibration cases separate from the agent regression dataset when their contracts differ.
- **Regression datasets:** Preserve specific failures a team would want to catch again, alongside enough ordinary cases to detect regressions elsewhere. State expected outcomes in the task's actual schema. Do not mix whole traces, tool calls, and final answers in one dataset unless they truly share an evaluation contract.
- **Scorers and experiments:** Score observable behavior against the dataset contract. Use the same cases for baseline and candidate runs. A useful comparison shows what improved, what stayed the same, and what may have regressed.

Working result: facets, datasets, scorers, and experiments with clear jobs in the same investigation. Add only the resources the requested story needs.

### 7. Inspect and revise the result

Validate the source, then preview a varied sample before writing data. For a live walkthrough, confirm that a viewer can enter through ordinary traffic and explore freely. For a technical proof of value, inspect the input/output contract, scorer behavior, and representative failures. In both cases, read ordinary traces before minor issues and rare failures. Check whether the world stays coherent, whether the trace explains each outcome, and whether a person could discover patterns without being handed the answer. Compare distinct intents, wording, paths, and outcomes across sampled traces. Changes to names or IDs alone do not provide meaningful variation. If most traces repeat the same interaction, expand the scenario pool. Check that sampled issue frequency fits the intended population. Remove placeholders, repeated scripts, contradictory fields, and labels that make the population look staged.

If the result feels forced, revisit the earliest phase that caused it. Weak context needs better grounding, repetitive traces need broader scenarios, and unconvincing discoveries need better underlying evidence. Revise the shape and inspect another sample before a live write.

"#;

pub const PRESENTATION: &str = r#"## Braintrust trace presentation

Use this reference when the shape should look like a real instrumented agent in Braintrust. It supplements the demo workflow and can also help with trace-focused work that does not need a demo story.

### Names and hierarchy

Name the root for the application's entrypoint. Name child spans for stable operations, such as `turn`, `lookup_account`, and `draft_reply`. Repeated turns can share the name `turn`; their order comes from the trace, not names such as `turn_1` and `turn_2`. Name tools for the functions actually called and LLM spans for their call type. Nest each call under the operation responsible for it. Keep the tree deep enough to explain decisions, but avoid wrapper spans that add no useful input, output, or boundary.

### Inputs, outputs, and causality

Give each span the input it consumed and the output it produced. Carry facts through the workflow with block references. A tool result should inform the decision and answer that follow it. Inside `repeat`, select authored turns by `repeat.index` and include earlier turns in later LLM inputs. If a failure changes the answer, gate its error, retry, and escalation together. A person should be able to follow the cause of the final answer through the tree.

Use chat messages for LLM inputs and an assistant message for the output so the conversation renders usefully. Put model parameters on LLM spans and arguments and results on tool spans. Keep model and provider consistent within a trace unless the workflow actually changes them.

### Metrics and labels

Use durations and token counts that fit the content. Derive counts with `tokens()` or keep sampled `prompt_tokens + completion_tokens = tokens` exactly. Vary leaf durations, keep `time_to_first_token` below its LLM span duration, and make parent timing contain the work it represents. Put meaningful operational tags on the root. Do not add `demo`, `synthetic`, or similar provenance tags or metadata unless the user explicitly requests them.

Inspect the rendered result, not just the source. Check that names, nesting, chat messages, tool payloads, timing, and final output help a viewer understand what happened without a spoken explanation.

"#;
