use super::{Attachment, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use uuid::Uuid;

pub fn stable_uuid(key: &str) -> Uuid {
    fn hash(bytes: &[u8], seed: u64) -> u64 {
        bytes
            .iter()
            .fold(seed, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3))
    }
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&hash(key.as_bytes(), 0xcbf29ce484222325).to_be_bytes());
    bytes[8..].copy_from_slice(&hash(key.as_bytes(), 0x84222325cbf29ce4).to_be_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x80; // UUID version 8: custom deterministic payload
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventKind {
    Task,
    Llm,
    Tool,
    Function,
    Scorer,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Task => "task",
            EventKind::Llm => "llm",
            EventKind::Tool => "tool",
            EventKind::Function => "function",
            EventKind::Scorer => "scorer",
        }
    }
}

/// Generated events and the local attachments needed to deliver them.
///
/// Span IDs must be unique within the batch. Every root and parent reference must
/// identify an event in the same batch, and `trace_count` must match its roots.
/// Local attachments awaiting upload must have entries in `attachments` with
/// matching keys. References reused from the destination do not need uploads.
/// Callers modifying a generated batch must preserve these relationships.
#[derive(Debug, Clone, PartialEq)]
pub struct EventBatch {
    pub events: Box<[Event]>,

    pub trace_count: usize,

    pub attachments: Box<[Attachment]>,
}

impl EventBatch {
    /// Replaces row and span IDs deterministically, preserving event order and relationships.
    ///
    /// # Panics
    ///
    /// Panics if a root or parent span ID does not belong to this batch.
    pub fn assign_stable_ids(&mut self, key: &str) {
        let old_to_new: HashMap<String, String> = self
            .events
            .iter()
            .enumerate()
            .map(|(index, event)| (event.span_id.clone(), stable_uuid(&format!("{key}/span/{index}")).to_string()))
            .collect();
        for (index, event) in self.events.iter_mut().enumerate() {
            event.id = stable_uuid(&format!("{key}/row/{index}")).to_string();
            event.span_id = old_to_new[&event.span_id].clone();
            event.root_span_id = old_to_new[&event.root_span_id].clone();
            for parent in event.span_parents.iter_mut() {
                *parent = old_to_new[parent].clone();
            }
        }
    }

    /// Resolves local paths relative to the source file and deduplicates uploads.
    ///
    /// This updates attachment keys and typed references without reading files.
    /// Errors determining the absolute path or representing it as UTF-8 are
    /// returned to the caller. A failure may leave the batch partially updated.
    pub fn resolve_attachment_paths(&mut self, shape_file: &Path) -> std::io::Result<()> {
        if self.attachments.is_empty() {
            return Ok(());
        }

        let shape_file = std::path::absolute(shape_file)?;
        let shape_dir = shape_file.parent().expect("an absolute shape path has a parent");
        let mut uploads: HashMap<(PathBuf, String), String> = HashMap::new();
        let mut resolved = HashMap::new();
        let mut distinct = Vec::new();
        for mut attachment in std::mem::take(&mut self.attachments).into_vec() {
            let generated_key = attachment.key.clone();
            let path = Path::new(&attachment.path);
            if path.is_relative() {
                attachment.path = shape_dir
                    .join(path)
                    .into_os_string()
                    .into_string()
                    .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "attachment path is not UTF-8"))?;
            }
            let identity = (PathBuf::from(&attachment.path), attachment.content_type.clone());
            let key = if let Some(key) = uploads.get(&identity) {
                key.clone()
            } else {
                let key = Uuid::new_v4().to_string();
                uploads.insert(identity, key.clone());
                attachment.key = key.clone();
                distinct.push(attachment.clone());
                key
            };
            attachment.key = key;
            resolved.insert(generated_key, attachment);
        }
        self.attachments = distinct.into_boxed_slice();
        for event in &mut self.events {
            for value in [&mut event.input, &mut event.output, &mut event.expected, &mut event.error]
                .into_iter()
                .flatten()
            {
                resolve_attachment_references(value, &resolved);
            }
            if let Some(metadata) = &mut event.metadata {
                for value in metadata.values_mut() {
                    resolve_attachment_references(value, &resolved);
                }
            }
        }
        Ok(())
    }

    /// Replaces generated keys with existing remote keys and removes their uploads.
    ///
    /// Each replacement must refer to an attachment already available at the
    /// destination. Unmapped attachments remain in the upload list.
    pub fn reuse_attachments(&mut self, keys: &HashMap<String, String>) {
        self.attachments = std::mem::take(&mut self.attachments)
            .into_vec()
            .into_iter()
            .filter(|attachment| !keys.contains_key(&attachment.key))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        for event in &mut self.events {
            for value in [&mut event.input, &mut event.output, &mut event.expected, &mut event.error]
                .into_iter()
                .flatten()
            {
                replace_attachment_keys(value, keys);
            }
            if let Some(metadata) = &mut event.metadata {
                for value in metadata.values_mut() {
                    replace_attachment_keys(value, keys);
                }
            }
        }
    }

    pub fn event_count(&self) -> usize {
        self.events.len()
    }

    pub fn trace_count(&self) -> usize {
        self.trace_count
    }
}

fn resolve_attachment_references(value: &mut Value, resolved: &HashMap<String, Attachment>) {
    match value {
        Value::Attachment(attachment) => {
            if let Some(replacement) = resolved.get(&attachment.key) {
                attachment.clone_from(replacement);
            }
        }
        Value::Object(fields) => {
            for value in fields.values_mut() {
                resolve_attachment_references(value, resolved);
            }
        }
        Value::Array(values) => values
            .iter_mut()
            .for_each(|value| resolve_attachment_references(value, resolved)),
        _ => {}
    }
}

fn replace_attachment_keys(value: &mut Value, keys: &HashMap<String, String>) {
    match value {
        Value::Attachment(attachment) => {
            if let Some(key) = keys.get(&attachment.key) {
                attachment.key.clone_from(key);
            }
        }
        Value::Object(fields) => {
            for value in fields.values_mut() {
                replace_attachment_keys(value, keys);
            }
        }
        Value::Array(values) => values.iter_mut().for_each(|value| replace_attachment_keys(value, keys)),
        _ => {}
    }
}

/// A generated event before conversion to a provider's format.
///
/// The time interval must be ordered, and IDs and attachment references must
/// satisfy the containing [`EventBatch`]'s invariants. Formats may impose
/// additional constraints, such as supported timestamp ranges.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub id: String,
    pub span_id: String,
    pub root_span_id: String,
    pub span_parents: Box<[String]>,
    pub name: String,
    pub kind: EventKind,
    pub start: SystemTime,
    pub end: SystemTime,
    pub input: Option<Value>,
    pub output: Option<Value>,
    pub expected: Option<Value>,
    pub error: Option<Value>,
    pub metadata: Option<BTreeMap<String, Value>>,
    pub scores: Option<BTreeMap<String, Value>>,
    pub metrics: BTreeMap<String, Value>,
    pub tags: Box<[String]>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::semantics::compile;
    use crate::mocking::data::{Distribution, generate};
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn resolved_attachment_values_match_the_upload_metadata() {
        struct Paths;
        impl crate::mocking::data::AttachmentFormat for Paths {
            fn attachment(&self, attachment: &Attachment) -> Option<serde_json::Value> {
                Some(serde_json::json!({"path": attachment.path, "key": attachment.key}))
            }
        }

        let model = compile(
            r#"
            trace "example" {
                input = { files = [attachment("report.txt", "text/plain")] }
                output = self.input["files"][0]
                metadata = { copied = self.output }
            }
            "#,
        )
        .unwrap();
        let mut batch = generate(
            model,
            1,
            Duration::from_secs(60),
            Distribution::Linear,
            UNIX_EPOCH + Duration::from_secs(7200),
            0,
            &(),
        )
        .unwrap();
        let shape = std::env::temp_dir().join("bts-attachment-values/shape.bt");
        batch.resolve_attachment_paths(&shape).unwrap();
        let upload = &batch.attachments[0];
        assert_eq!(Path::new(&upload.path), shape.parent().unwrap().join("report.txt"));
        let expected = serde_json::json!({"path": upload.path, "key": upload.key});
        let event = &batch.events[0];
        assert_eq!(event.input.as_ref().unwrap()["files"][0].project(&Paths).unwrap(), expected);
        assert_eq!(event.output.as_ref().unwrap().project(&Paths).unwrap(), expected);
        assert_eq!(event.metadata.as_ref().unwrap()["copied"].project(&Paths).unwrap(), expected);
    }
}
