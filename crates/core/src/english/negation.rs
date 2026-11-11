// Auxiliary, expanded negation, straight contraction, and curly contraction.
pub(super) const FORMS: &[(&str, &str, &str, &str)] = &[
    ("can", "cannot", "can't", "can’t"),
    ("could", "could not", "couldn't", "couldn’t"),
    ("must", "must not", "mustn't", "mustn’t"),
    ("should", "should not", "shouldn't", "shouldn’t"),
    ("will", "will not", "won't", "won’t"),
    ("would", "would not", "wouldn't", "wouldn’t"),
    ("do", "do not", "don't", "don’t"),
    ("does", "does not", "doesn't", "doesn’t"),
    ("did", "did not", "didn't", "didn’t"),
    ("is", "is not", "isn't", "isn’t"),
    ("are", "are not", "aren't", "aren’t"),
    ("was", "was not", "wasn't", "wasn’t"),
    ("were", "were not", "weren't", "weren’t"),
    ("have", "have not", "haven't", "haven’t"),
    ("has", "has not", "hasn't", "hasn’t"),
    ("had", "had not", "hadn't", "hadn’t"),
];

pub(super) fn contract(auxiliary: &str) -> Option<&'static str> {
    FORMS
        .iter()
        .find_map(|&(base, _, contracted, _)| (base == auxiliary).then_some(contracted))
}

pub(super) fn expand(contraction: &str) -> Option<(&'static str, &'static str)> {
    FORMS.iter().find_map(|&(base, expanded, straight, curly)| {
        (contraction == straight || contraction == curly).then_some((base, expanded))
    })
}
