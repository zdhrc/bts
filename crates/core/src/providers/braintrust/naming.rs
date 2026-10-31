// braintrust function slugs: lowercase alphanumeric runs joined by hyphens
pub(crate) fn slugify(name: &str) -> Option<String> {
    let mut slug = String::with_capacity(name.len());
    let mut gap = false;
    for part in name.chars() {
        if part.is_ascii_alphanumeric() {
            if gap && !slug.is_empty() {
                slug.push('-');
            }
            gap = false;
            slug.push(part.to_ascii_lowercase());
        } else {
            gap = true;
        }
    }
    (!slug.is_empty()).then_some(slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugifies_names() {
        assert_eq!(slugify("response-quality").as_deref(), Some("response-quality"));
        assert_eq!(slugify("Answer Quality!").as_deref(), Some("answer-quality"));
        assert_eq!(slugify("answer_quality").as_deref(), Some("answer-quality"));
        assert_eq!(slugify("  spaced   out  ").as_deref(), Some("spaced-out"));
        assert_eq!(slugify("v2 scorer").as_deref(), Some("v2-scorer"));
        assert_eq!(slugify("!!!"), None);
        assert_eq!(slugify(""), None);
    }
}
