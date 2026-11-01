//! Generated adapters between agent execution scopes and Braintrust spans.

use crate::dsl::ScorerLang;

pub(super) fn runtime(lang: ScorerLang) -> &'static str {
    match lang {
        ScorerLang::Python => PYTHON,
        ScorerLang::Typescript => TYPESCRIPT,
    }
}

const PYTHON: &str = r#"
def start_trace(parent, name, kind):
    import time
    return parent.start_span(name=name, type=kind, start_time=time.time())

def log_trace(span, fields):
    span.log(**fields)

def end_trace(span):
    import time
    span.end(end_time=time.time())
"#;

const TYPESCRIPT: &str = r#"
function startTrace(parent: any, name: string, kind: string): any {
    return parent.startSpan({name, type: kind, startTime: Date.now() / 1000});
}
function logTrace(span: any, fields: Record<string, unknown>): void {
    span.log(fields);
}
function endTrace(span: any): void {
    span.end({endTime: Date.now() / 1000});
}
"#;
