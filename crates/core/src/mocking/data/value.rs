use super::contracts::AttachmentFormat;
use serde_json::{Number, Value as JsonValue};
use std::{collections::BTreeMap, fmt, ops::Index};

/// A local attachment collected while evaluating a behavior spec.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Attachment {
    pub path: String,
    pub filename: String,
    pub content_type: String,
    pub key: String,
}

/// A generated value that preserves attachments independently of their encoding.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Value {
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    Array(Vec<Value>),
    Object(BTreeMap<String, Value>),
    Attachment(Attachment),
}

impl Value {
    /// Projects nested attachment references using the selected format.
    pub fn project(&self, format: &dyn AttachmentFormat) -> Result<JsonValue, UnsupportedAttachment> {
        Ok(match self {
            Self::Null => JsonValue::Null,
            Self::Bool(value) => JsonValue::Bool(*value),
            Self::Number(value) => JsonValue::Number(value.clone()),
            Self::String(value) => JsonValue::String(value.clone()),
            Self::Array(values) => {
                JsonValue::Array(values.iter().map(|value| value.project(format)).collect::<Result<_, _>>()?)
            }
            Self::Object(values) => JsonValue::Object(
                values
                    .iter()
                    .map(|(key, value)| Ok((key.clone(), value.project(format)?)))
                    .collect::<Result<_, _>>()?,
            ),
            Self::Attachment(attachment) => format.attachment(attachment).ok_or(UnsupportedAttachment)?,
        })
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Number(value) => value.as_i64(),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Number(value) => value.as_u64(),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(value) => value.as_f64(),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Self::Array(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&BTreeMap<String, Value>> {
        match self {
            Self::Object(value) => Some(value),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_object()?.get(key)
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
}

impl From<JsonValue> for Value {
    fn from(value: JsonValue) -> Self {
        match value {
            JsonValue::Null => Self::Null,
            JsonValue::Bool(value) => Self::Bool(value),
            JsonValue::Number(value) => Self::Number(value),
            JsonValue::String(value) => Self::String(value),
            JsonValue::Array(values) => Self::Array(values.into_iter().map(Self::from).collect()),
            JsonValue::Object(values) => {
                Self::Object(values.into_iter().map(|(key, value)| (key, Self::from(value))).collect())
            }
        }
    }
}

macro_rules! from_json {
    ($($ty:ty),* $(,)?) => {
        $(impl From<$ty> for Value {
            fn from(value: $ty) -> Self {
                JsonValue::from(value).into()
            }
        })*
    };
}

from_json!(
    bool, String, &str, i8, i16, i32, i64, isize, u8, u16, u32, u64, usize, f32, f64
);

impl<T: Into<Value>> From<Vec<T>> for Value {
    fn from(values: Vec<T>) -> Self {
        Self::Array(values.into_iter().map(Into::into).collect())
    }
}

impl Index<&str> for Value {
    type Output = Value;

    fn index(&self, key: &str) -> &Self::Output {
        self.get(key).unwrap_or(&Value::Null)
    }
}

impl Index<usize> for Value {
    type Output = Value;

    fn index(&self, index: usize) -> &Self::Output {
        self.as_array().and_then(|values| values.get(index)).unwrap_or(&Value::Null)
    }
}

/// The selected format has no representation for attachments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedAttachment;

impl fmt::Display for UnsupportedAttachment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the selected format does not support attachments")
    }
}

impl std::error::Error for UnsupportedAttachment {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Format;

    impl AttachmentFormat for Format {
        fn attachment(&self, attachment: &Attachment) -> Option<JsonValue> {
            Some(json!({"file": attachment.filename, "reference": attachment.key}))
        }
    }

    #[test]
    fn projects_only_typed_attachments() {
        let ordinary = json!({
            "type": "braintrust_attachment",
            "filename": "report.txt",
            "content_type": "text/plain",
            "key": "local"
        });
        let attachment = Attachment {
            path: "report.txt".to_owned(),
            filename: "report.txt".to_owned(),
            content_type: "text/plain".to_owned(),
            key: "local".to_owned(),
        };
        let value = Value::Array(vec![ordinary.clone().into(), Value::Attachment(attachment)]);
        assert_eq!(
            value.project(&Format).unwrap(),
            json!([ordinary, {"file": "report.txt", "reference": "local"}])
        );
        assert_eq!(value.project(&()).unwrap_err(), UnsupportedAttachment);
        assert_eq!(Value::from(ordinary.clone()).project(&()).unwrap(), ordinary);
    }
}
