pub(crate) mod builder;
pub(crate) mod client;
mod component;
mod diff;
mod emit;

use crate::conf::Braintrust;
use crate::dsl::{Scorer, ScorerKind, ScorerLang};
use std::collections::HashMap;
use std::fmt;

pub(crate) use component::{Component, ComponentKind, Payload};

// matches autoevals classifiers until the dsl exposes it
const JUDGE_USE_COT: bool = true;

// slugs parallel to the scorers, unique across the whole set
fn resolve_slugs(scorers: &[Scorer]) -> Result<Vec<String>, Error> {
    let mut slugs = Vec::with_capacity(scorers.len());
    let mut first_by_slug: HashMap<String, String> = HashMap::new();
    for scorer in scorers {
        let slug = component::slugify(&scorer.name).ok_or_else(|| Error::UnsluggableName {
            name: scorer.name.clone(),
        })?;
        if let Some(first) = first_by_slug.insert(slug.clone(), scorer.name.clone()) {
            return Err(Error::DuplicateSlug {
                slug,
                first,
                second: scorer.name.clone(),
            });
        }
        slugs.push(slug);
    }
    Ok(slugs)
}

// the flag beats the block's lang beats the python default
fn resolve_lang(scorer: &Scorer, lang: Option<ScorerLang>) -> ScorerLang {
    lang.or(scorer.lang).unwrap_or(ScorerLang::Python)
}

fn extension(lang: ScorerLang) -> &'static str {
    match lang {
        ScorerLang::Python => "py",
        ScorerLang::Typescript => "ts",
    }
}

// pure: modeled scorers -> pushable components; codegen happens here. push is
// one braintrust function per scorer, so packing never applies
pub(crate) fn plan(scorers: &[Scorer], lang: Option<ScorerLang>) -> Result<Vec<Component>, Error> {
    let slugs = resolve_slugs(scorers)?;
    let mut components = Vec::with_capacity(scorers.len());

    for (scorer, slug) in scorers.iter().zip(slugs) {
        let payload = match &scorer.kind {
            ScorerKind::Code { score, whens } => {
                let lang = resolve_lang(scorer, lang);
                let item = emit::Item {
                    name: &scorer.name,
                    slug: &slug,
                    score,
                    whens,
                };
                let code = emit::module(lang, &[item], true).map_err(Error::Emit)?;
                Payload::Code { lang, code }
            }
            ScorerKind::Judge { model, prompt, options } => Payload::Prompt {
                model: model.clone(),
                content: component::mustache(prompt),
                choice_scores: options.iter().map(|option| (option.label.clone(), option.score)).collect(),
                use_cot: JUDGE_USE_COT,
            },
        };

        components.push(Component {
            name: scorer.name.clone(),
            slug,
            kind: ComponentKind::Scorer,
            payload,
        });
    }

    Ok(components)
}

// pure: modeled scorers -> packed source files for the builder; the grouping
// key is the file stem, a scorer's own slug when unset
pub(crate) fn assemble(scorers: &[Scorer], lang: Option<ScorerLang>) -> Result<Assembly, Error> {
    let slugs = resolve_slugs(scorers)?;

    // groups keep first-appearance order
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    let mut judges = Vec::new();
    for (index, (scorer, slug)) in scorers.iter().zip(&slugs).enumerate() {
        if matches!(scorer.kind, ScorerKind::Judge { .. }) {
            judges.push(slug.clone());
            continue;
        }
        let stem = scorer.file.clone().unwrap_or_else(|| slug.clone());
        match groups.iter_mut().find(|(existing, _)| *existing == stem) {
            Some((_, members)) => members.push(index),
            None => groups.push((stem, vec![index])),
        }
    }

    let mut files = Vec::with_capacity(groups.len());
    for (stem, members) in groups {
        let file_lang = resolve_lang(&scorers[members[0]], lang);
        if let Some(&conflict) = members
            .iter()
            .find(|&&member| resolve_lang(&scorers[member], lang) != file_lang)
        {
            return Err(Error::PackedLangConflict {
                file: stem,
                first: scorers[members[0]].name.clone(),
                second: scorers[conflict].name.clone(),
            });
        }

        let items: Vec<emit::Item> = members
            .iter()
            .map(|&member| {
                let ScorerKind::Code { score, whens } = &scorers[member].kind else {
                    unreachable!("judges were filtered out of the groups");
                };
                emit::Item {
                    name: &scorers[member].name,
                    slug: &slugs[member],
                    score,
                    whens,
                }
            })
            .collect();
        let contents = emit::module(file_lang, &items, false).map_err(Error::Emit)?;

        files.push(SourceFile {
            // the kind suffix is the pattern future component kinds follow
            name: format!("{stem}.{}.{}", ComponentKind::Scorer.function_type(), extension(file_lang)),
            contents,
            slugs: members.iter().map(|&member| slugs[member].clone()).collect(),
        });
    }

    Ok(Assembly { files, judges })
}

pub(crate) struct Assembly {
    pub(crate) files: Vec<SourceFile>,
    // judge slugs, skipped because they push as prompt functions
    pub(crate) judges: Vec<String>,
}

pub(crate) struct SourceFile {
    // full file name, kind suffix and extension included
    pub(crate) name: String,
    pub(crate) contents: String,
    // the scorers packed into the file, for reporting
    pub(crate) slugs: Vec<String>,
}

// local: write the assembled sources under out, mirroring sdg::write
pub(crate) fn build(assembly: &Assembly, out: &std::path::Path) -> Result<Vec<builder::Built>, builder::Error> {
    builder::build(assembly, out)
}

// networked: look up each slug, diff, and (unless dry_run) create-or-replace
pub(crate) fn reconcile(config: &Braintrust, components: &[Component], dry_run: bool) -> Result<Vec<Outcome>, client::Error> {
    let client = client::Client::new(config)?;
    let mut outcomes = Vec::with_capacity(components.len());

    for component in components {
        let lang = match &component.payload {
            Payload::Code { lang, .. } => Some(*lang),
            Payload::Prompt { .. } => None,
        };
        let (action, id) = match client.lookup(&component.slug)? {
            None => {
                let id = if dry_run { None } else { Some(client.upsert(component)?.id) };
                (Action::Create, id)
            }
            Some(remote) => match diff::changed(component, &remote) {
                None => (Action::Unchanged, Some(remote.id)),
                Some(diff) => {
                    let id = if dry_run {
                        Some(remote.id)
                    } else {
                        Some(client.upsert(component)?.id)
                    };
                    (Action::Update { diff }, id)
                }
            },
        };
        outcomes.push(Outcome {
            name: component.name.clone(),
            slug: component.slug.clone(),
            lang,
            action,
            id,
        });
    }

    Ok(outcomes)
}

pub(crate) struct Outcome {
    pub(crate) name: String,
    pub(crate) slug: String,
    // none for judges, which push as prompt functions
    pub(crate) lang: Option<ScorerLang>,
    pub(crate) action: Action,
    pub(crate) id: Option<String>,
}

pub(crate) enum Action {
    Create,
    Update { diff: String },
    Unchanged,
}

#[derive(Debug)]
pub(crate) enum Error {
    UnsluggableName { name: String },
    DuplicateSlug { slug: String, first: String, second: String },
    PackedLangConflict { file: String, first: String, second: String },
    Emit(emit::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsluggableName { name } => {
                write!(formatter, "scorer name \"{name}\" has no alphanumeric characters to slug")
            }
            Self::DuplicateSlug { slug, first, second } => {
                write!(
                    formatter,
                    "scorers \"{first}\" and \"{second}\" both push as slug `{slug}`; rename one"
                )
            }
            Self::PackedLangConflict { file, first, second } => {
                write!(
                    formatter,
                    "scorers \"{first}\" and \"{second}\" pack into file `{file}` but resolve to different languages"
                )
            }
            Self::Emit(source) => source.fmt(formatter),
        }
    }
}

impl std::error::Error for Error {}

// a scripted http server shared by the scg tests, mirroring the writer's
#[cfg(test)]
pub(crate) mod testutil {
    use reqwest::StatusCode;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc::{self, Receiver};
    use std::thread;
    use std::time::Duration;

    pub(crate) fn serve(responses: Vec<(StatusCode, String)>) -> (String, Receiver<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel();

        thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&mut stream);
                sender.send(request).unwrap();

                let reason = status.canonical_reason().unwrap_or("Unknown");
                write!(
                    stream,
                    "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status.as_u16(),
                    reason,
                    body.len(),
                    body,
                )
                .unwrap();
            }
        });

        (format!("http://{address}"), receiver)
    }

    fn read_request(stream: &mut TcpStream) -> Vec<u8> {
        stream.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 4096];

        loop {
            let read = stream.read(&mut buffer).unwrap();
            request.extend_from_slice(&buffer[..read]);

            let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
                continue;
            };
            let body_start = header_end + 4;
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(str::trim)
                        .map(str::to_owned)
                })
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or_default();

            if request.len() >= body_start + content_length {
                return request;
            }
        }
    }

    pub(crate) fn split_request(request: &[u8]) -> (&str, &[u8]) {
        let header_end = request.windows(4).position(|window| window == b"\r\n\r\n").unwrap();
        let headers = std::str::from_utf8(&request[..header_end + 4]).unwrap();
        (headers, &request[header_end + 4..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::compile;
    use reqwest::StatusCode;
    use serde_json::{Value as JsonValue, json};
    use std::time::Duration;
    use uuid::Uuid;

    fn scorers(source: &str) -> Vec<Scorer> {
        compile(source).unwrap().scorers
    }

    fn config(api_url: String) -> Braintrust {
        let mut config = Braintrust::new("secret".to_owned(), Uuid::new_v4());
        config.api_url = api_url;
        config.request_timeout = Duration::from_secs(1);
        config
    }

    const CODE_SCORER: &str = r#"
        scorer "response-quality" {
            lang = "python"
            code {
                score = clamp(normal(0.78, 0.12), 0, 1)
                when {
                    cond = output == null
                    score = 0
                }
            }
        }
    "#;

    fn code_component() -> Vec<Component> {
        let scorers = scorers(CODE_SCORER);
        plan(&scorers, None).unwrap()
    }

    fn remote_matching(component: &Component) -> JsonValue {
        let Payload::Code { code, .. } = &component.payload else {
            panic!("expected a code payload");
        };
        json!({
            "id": "fn-1",
            "name": component.name,
            "slug": component.slug,
            "function_data": {
                "type": "code",
                "data": {
                    "type": "inline",
                    "runtime_context": { "runtime": "python", "version": "3.11" },
                    "code": code,
                },
            },
        })
    }

    #[test]
    fn plans_components_from_scorers() {
        let components = code_component();
        assert_eq!(components.len(), 1);
        assert_eq!(components[0].slug, "response-quality");
        let Payload::Code { lang, code } = &components[0].payload else {
            panic!("expected a code payload");
        };
        assert_eq!(*lang, ScorerLang::Python);
        assert!(code.contains("def scorer_response_quality(input, output, expected, metadata):"));
        assert!(code.contains("handler = scorer_response_quality"));
    }

    #[test]
    fn lang_override_beats_the_block() {
        let scorers = scorers(CODE_SCORER);
        let components = plan(&scorers, Some(ScorerLang::Typescript)).unwrap();
        let Payload::Code { lang, code } = &components[0].payload else {
            panic!("expected a code payload");
        };
        assert_eq!(*lang, ScorerLang::Typescript);
        assert!(code.contains("function scorer_response_quality("));
        assert!(code.contains("const handler = scorer_response_quality;"));
    }

    #[test]
    fn assembles_packed_and_default_stems() {
        let scorers = scorers(
            r#"
            scorer "a" { file = "shared" code { score = 0.5 } }
            scorer "b" { file = "shared" code { score = 0.6 } }
            scorer "c" { code { score = 0.7 } }
            scorer "j" { judge { model = "m" prompt = "p" options = { x = 1, y = 0 } } }
            "#,
        );
        let assembly = assemble(&scorers, None).unwrap();

        assert_eq!(assembly.files.len(), 2);
        assert_eq!(assembly.files[0].name, "shared.scorer.py");
        assert_eq!(assembly.files[0].slugs, ["a", "b"]);
        assert!(assembly.files[0].contents.contains("def scorer_a(") && assembly.files[0].contents.contains("def scorer_b("));
        // pushed handler aliases never appear in built files
        assert!(!assembly.files[0].contents.contains("handler"));
        assert_eq!(assembly.files[1].name, "c.scorer.py");
        assert_eq!(assembly.judges, ["j"]);
    }

    #[test]
    fn rejects_packed_language_conflicts() {
        let scorers = scorers(
            r#"
            scorer "a" { lang = "python" file = "shared" code { score = 0.5 } }
            scorer "b" { lang = "typescript" file = "shared" code { score = 0.6 } }
            "#,
        );
        assert!(matches!(
            assemble(&scorers, None),
            Err(Error::PackedLangConflict { file, .. }) if file == "shared"
        ));

        // the override forces one language, dissolving the conflict
        let assembly = assemble(&scorers, Some(ScorerLang::Typescript)).unwrap();
        assert_eq!(assembly.files[0].name, "shared.scorer.ts");
    }

    #[test]
    fn plans_judges_as_prompt_payloads() {
        let scorers = scorers(include_str!("../../examples/judge_scorer.bt"));
        let components = plan(&scorers, None).unwrap();
        let Payload::Prompt {
            model,
            content,
            choice_scores,
            use_cot,
        } = &components[0].payload
        else {
            panic!("expected a prompt payload");
        };
        assert_eq!(model, "gpt-4o-mini");
        assert!(content.contains("{{input}}") && content.contains("{{output}}"));
        assert_eq!(choice_scores[0], ("excellent".to_owned(), 1.0));
        assert!(use_cot);
    }

    #[test]
    fn rejects_duplicate_slugs() {
        let scorers = scorers(
            r#"
            scorer "answer quality" { code { score = 0.5 } }
            scorer "answer-quality" { code { score = 0.7 } }
            "#,
        );
        assert!(matches!(plan(&scorers, None), Err(Error::DuplicateSlug { .. })));
    }

    #[test]
    fn creates_a_missing_scorer() {
        let components = code_component();
        let (api_url, requests) = testutil::serve(vec![
            (StatusCode::OK, r#"{"objects": []}"#.to_owned()),
            (StatusCode::OK, remote_matching(&components[0]).to_string()),
        ]);

        let outcomes = reconcile(&config(api_url), &components, false).unwrap();

        assert!(matches!(outcomes[0].action, Action::Create));
        assert_eq!(outcomes[0].id.as_deref(), Some("fn-1"));

        let lookup = requests.recv().unwrap();
        let (headers, _) = testutil::split_request(&lookup);
        assert!(headers.starts_with("GET /v1/function?"), "headers:\n{headers}");
        assert!(headers.contains("slug=response-quality"));
        assert!(headers.to_ascii_lowercase().contains("authorization: bearer secret"));

        let upsert = requests.recv().unwrap();
        let (headers, body) = testutil::split_request(&upsert);
        assert!(headers.starts_with("PUT /v1/function"), "headers:\n{headers}");
        let body: JsonValue = serde_json::from_slice(body).unwrap();
        assert_eq!(body["slug"], "response-quality");
        assert_eq!(body["function_type"], "scorer");
        assert_eq!(body["function_data"]["data"]["type"], "inline");
        assert!(
            body["function_data"]["data"]["code"]
                .as_str()
                .unwrap()
                .contains("def scorer_response_quality(")
        );
    }

    #[test]
    fn leaves_a_matching_scorer_unchanged() {
        let components = code_component();
        let listing = json!({ "objects": [remote_matching(&components[0])] });
        // one scripted response: an unchanged scorer must not send a put
        let (api_url, _requests) = testutil::serve(vec![(StatusCode::OK, listing.to_string())]);

        let outcomes = reconcile(&config(api_url), &components, false).unwrap();

        assert!(matches!(outcomes[0].action, Action::Unchanged));
        assert_eq!(outcomes[0].id.as_deref(), Some("fn-1"));
    }

    #[test]
    fn updates_a_stale_scorer_with_a_diff() {
        let components = code_component();
        let mut remote = remote_matching(&components[0]);
        let stale = remote["function_data"]["data"]["code"]
            .as_str()
            .unwrap()
            .replace("0.78", "0.5");
        remote["function_data"]["data"]["code"] = json!(stale);
        let listing = json!({ "objects": [remote.clone()] });
        let (api_url, _requests) = testutil::serve(vec![
            (StatusCode::OK, listing.to_string()),
            (StatusCode::OK, remote.to_string()),
        ]);

        let outcomes = reconcile(&config(api_url), &components, false).unwrap();

        let Action::Update { diff } = &outcomes[0].action else {
            panic!("expected an update");
        };
        assert!(diff.contains("- ") && diff.contains("0.5"), "diff:\n{diff}");
        assert!(diff.contains("+ ") && diff.contains("0.78"), "diff:\n{diff}");
    }

    #[test]
    fn dry_run_never_writes() {
        let components = code_component();
        // only the lookup is scripted; a put would fail the run
        let (api_url, _requests) = testutil::serve(vec![(StatusCode::OK, r#"{"objects": []}"#.to_owned())]);

        let outcomes = reconcile(&config(api_url), &components, true).unwrap();

        assert!(matches!(outcomes[0].action, Action::Create));
        assert_eq!(outcomes[0].id, None);
    }

    #[test]
    fn pushes_judges_as_prompt_functions() {
        let scorers = scorers(include_str!("../../examples/judge_scorer.bt"));
        let components = plan(&scorers, None).unwrap();
        let remote = json!({ "id": "fn-2", "name": "helpfulness", "slug": "helpfulness" });
        let (api_url, requests) = testutil::serve(vec![
            (StatusCode::OK, r#"{"objects": []}"#.to_owned()),
            (StatusCode::OK, remote.to_string()),
        ]);

        let outcomes = reconcile(&config(api_url), &components, false).unwrap();
        assert!(matches!(outcomes[0].action, Action::Create));
        assert_eq!(outcomes[0].lang, None);

        let _lookup = requests.recv().unwrap();
        let upsert = requests.recv().unwrap();
        let (_, body) = testutil::split_request(&upsert);
        let body: JsonValue = serde_json::from_slice(body).unwrap();
        assert_eq!(body["function_data"]["type"], "prompt");
        assert_eq!(body["prompt_data"]["options"]["model"], "gpt-4o-mini");
        assert_eq!(body["prompt_data"]["parser"]["type"], "llm_classifier");
        assert_eq!(body["prompt_data"]["parser"]["choice_scores"]["excellent"], 1.0);
        let content = body["prompt_data"]["prompt"]["messages"][0]["content"].as_str().unwrap();
        assert!(content.contains("{{input}}"));
    }
}
