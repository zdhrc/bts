use super::{Client, Error, Response};
use reqwest::StatusCode;
use serde_json::Value;
use std::fmt;

/// A failure while requesting or interpreting a Braintrust API response.
#[derive(Debug)]
pub enum ApiError {
    Request(Error),
    Body {
        context: String,
        source: reqwest::Error,
    },
    Rejected {
        context: String,
        status: StatusCode,
        body: String,
    },
    MissingObjects {
        context: String,
    },
    Ambiguous {
        kind: String,
        name: String,
        count: usize,
    },
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request(source) => source.fmt(f),
            Self::Body { context, source } => write!(f, "{context}: {source}"),
            Self::Rejected { context, status, body } => write!(f, "{context}: HTTP {status}: {body}"),
            Self::MissingObjects { context } => write!(f, "{context} returned no objects array"),
            Self::Ambiguous { kind, name, .. } => write!(f, "multiple {kind}s matched {name:?}"),
        }
    }
}

impl std::error::Error for ApiError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Request(source) => Some(source),
            Self::Body { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub fn checked(response: Response, context: &str) -> Result<Value, ApiError> {
    let body_error = |source: reqwest::Error| ApiError::Body {
        context: context.to_owned(),
        source: source.without_url(),
    };
    let status = response.status();
    if !status.is_success() {
        return Err(ApiError::Rejected {
            context: context.to_owned(),
            status,
            body: response.text().map_err(body_error)?.chars().take(500).collect(),
        });
    }
    response.json().map_err(body_error)
}

pub(crate) fn get_objects(client: &Client, url: &str, query: &[(&str, &str)]) -> Result<Vec<Value>, ApiError> {
    let response = client.get(url).query(query).send().map_err(ApiError::Request)?;
    checked(response, url)?["objects"]
        .as_array()
        .cloned()
        .ok_or_else(|| ApiError::MissingObjects { context: url.to_owned() })
}

pub(crate) fn single(mut objects: Vec<Value>, kind: &str, name: &str) -> Result<Option<Value>, ApiError> {
    if objects.len() > 1 {
        return Err(ApiError::Ambiguous {
            kind: kind.to_owned(),
            name: name.to_owned(),
            count: objects.len(),
        });
    }
    Ok(objects.pop())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::braintrust::client::tests::{Reply, serve};
    use serde_json::json;
    use std::error::Error as _;

    #[test]
    fn preserves_rejection_status_and_bounds_the_body() {
        let (client, _) = serve(vec![Reply {
            status: 403,
            body: "x".repeat(600),
            headers: String::new(),
        }]);
        let error = checked(client.get("/test").send().unwrap(), "lookup scorer").unwrap_err();
        assert!(
            matches!(error, ApiError::Rejected { status: StatusCode::FORBIDDEN, context, body }
            if context == "lookup scorer" && body.len() == 500)
        );
    }

    #[test]
    fn distinguishes_invalid_json_from_an_invalid_object_list() {
        let (client, _) = serve(vec![
            Reply {
                status: 200,
                body: "not json".to_owned(),
                headers: String::new(),
            },
            Reply::json(json!({"objects": {}})),
        ]);
        let error = get_objects(&client, "/test", &[]).unwrap_err();
        assert!(matches!(error, ApiError::Body { .. }));
        assert!(error.source().is_some());
        assert!(matches!(
            get_objects(&client, "/test", &[]),
            Err(ApiError::MissingObjects { .. })
        ));
    }

    #[test]
    fn lookup_requires_an_unambiguous_result() {
        let (client, _) = serve(vec![Reply::json(json!({"objects": [{"id":"one"}, {"id":"two"}]}))]);
        let objects = get_objects(&client, "/test", &[]).unwrap();
        assert!(matches!(
            single(objects, "scorer", "quality"),
            Err(ApiError::Ambiguous { count: 2, .. })
        ));
        assert_eq!(
            single(vec![json!({"id":"one"})], "scorer", "quality").unwrap(),
            Some(json!({"id":"one"}))
        );
        assert!(single(vec![], "scorer", "quality").unwrap().is_none());
    }
}
