pub(in crate::mocking::code) mod python;
pub(crate) mod rendering;
pub(in crate::mocking::code) mod typescript;

use super::{Emitter, Item, Module};
use crate::dsl::ScorerLang;
use std::{collections::HashSet, fmt};

/// The built-in Python and TypeScript function emitters.
///
/// Slugs must be nonempty and unique within a call. They may contain only
/// lowercase ASCII letters, digits, and hyphens. These restrictions preserve
/// distinct function names when hyphens become underscores.
pub struct Languages;

impl Emitter for Languages {
    type Error = Error;

    fn emit(&self, lang: ScorerLang, items: &[Item<'_>]) -> Result<Module, Error> {
        let mut slugs = HashSet::with_capacity(items.len());
        for item in items {
            if item.slug.is_empty()
                || !item
                    .slug
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            {
                return Err(Error::InvalidSlug {
                    slug: item.slug.to_owned(),
                });
            }
            if !slugs.insert(item.slug) {
                return Err(Error::DuplicateSlug {
                    slug: item.slug.to_owned(),
                });
            }
        }
        match lang {
            ScorerLang::Python => python::module(items),
            ScorerLang::Typescript => typescript::module(items),
        }
    }
}

// validated slugs keep this mapping unique and safe in both languages
pub(super) fn fn_name(slug: &str) -> String {
    format!("scorer_{}", slug.replace('-', "_"))
}

#[derive(Debug)]
pub enum Error {
    InvalidSlug { slug: String },
    DuplicateSlug { slug: String },
    // the modeler rejects everything untranspilable, so this only fires on a
    // contract break between the dsl and code generation
    Unsupported { what: &'static str },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSlug { slug } => write!(
                formatter,
                "scorer slug `{slug}` must be nonempty and contain only lowercase ASCII letters, digits, and hyphens"
            ),
            Self::DuplicateSlug { slug } => write!(formatter, "multiple scorers use slug `{slug}` in one emitted module"),
            Self::Unsupported { what } => {
                write!(
                    formatter,
                    "scorer expression contains {what}, which cannot emit as scorer code"
                )
            }
        }
    }
}

impl std::error::Error for Error {}
