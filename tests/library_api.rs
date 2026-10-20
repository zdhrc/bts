use bts::api::{Distribution, GenerationOptions, generate};
use std::time::{Duration, UNIX_EPOCH};

#[test]
fn core_returns_unresolved_attachments_for_the_consumer_to_handle() {
    // No file exists at this path. The in-memory core must neither read it nor
    // impose a platform adapter's policy for handling attachments.
    let source = "trace \"t\" { input = attachment(\"missing.pdf\", \"application/pdf\") }";
    let batch = generate(
        source,
        GenerationOptions {
            count: 1,
            seed: u64::MAX,
            start: UNIX_EPOCH + Duration::from_secs(3600),
            end: UNIX_EPOCH + Duration::from_secs(7200),
            distribution: Distribution::Linear,
        },
    )
    .unwrap();
    assert!(batch.has_attachments());
    assert_eq!(serde_json::to_value(batch).unwrap()["events"].as_array().unwrap().len(), 1);
}
