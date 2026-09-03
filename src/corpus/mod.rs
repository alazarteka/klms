mod curate;
mod object_store;
mod query;
mod schema;
mod storage;
mod sync;

use std::{fmt, str::FromStr};

use crate::{error::AppError, reference::ResourceRef};

pub use crate::models::{
    ActivityEntry, ChangeEntry, ContentRecord, EditResult, HistoryEntry, LastSync, LibraryStatus,
    RelationResult, RetractionResult, SearchHit, SyncSummary,
};
pub use sync::SyncOptions;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LibraryRef {
    Course(String),
    Resource(String),
    Representation(i64),
    Sha256(String),
    Assertion(i64),
    Relation(i64),
    Sync(i64),
}

impl FromStr for LibraryRef {
    type Err = AppError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let numeric = |prefix: &str| {
            value
                .strip_prefix(prefix)
                .and_then(|id| id.parse::<i64>().ok())
                .filter(|id| *id > 0)
        };
        if let Some(id) = numeric("representation:") {
            return Ok(Self::Representation(id));
        }
        if let Some(id) = numeric("assertion:") {
            return Ok(Self::Assertion(id));
        }
        if let Some(id) = numeric("relation:") {
            return Ok(Self::Relation(id));
        }
        if let Some(id) = numeric("sync:") {
            return Ok(Self::Sync(id));
        }
        if let Some(id) = value.strip_prefix("course:").filter(|id| valid_id(id)) {
            return Ok(Self::Course(id.into()));
        }
        if let Some(hash) = value
            .strip_prefix("sha256:")
            .filter(|hash| valid_hash(hash, 64))
        {
            return Ok(Self::Sha256(hash.into()));
        }
        if valid_resource(value) {
            return Ok(Self::Resource(value.into()));
        }
        Err(AppError::usage(format!(
            "invalid library reference {value:?}"
        )))
    }
}

impl fmt::Display for LibraryRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Course(id) => write!(formatter, "course:{id}"),
            Self::Resource(reference) => formatter.write_str(reference),
            Self::Representation(id) => write!(formatter, "representation:{id}"),
            Self::Sha256(hash) => write!(formatter, "sha256:{hash}"),
            Self::Assertion(id) => write!(formatter, "assertion:{id}"),
            Self::Relation(id) => write!(formatter, "relation:{id}"),
            Self::Sync(id) => write!(formatter, "sync:{id}"),
        }
    }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn valid_hash(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_resource(value: &str) -> bool {
    if let Some(hash) = value.strip_prefix("resource:") {
        return valid_hash(hash, 24);
    }
    ResourceRef::parse(value).is_ok_and(|reference| !matches!(reference, ResourceRef::Course(_)))
}

pub struct Corpus {
    storage: storage::CorpusStorage,
}

impl Corpus {
    pub fn open() -> Result<Self, AppError> {
        Ok(Self {
            storage: storage::CorpusStorage::initialize()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_references_parse_and_display_round_trip() {
        for value in [
            "course:12",
            "file:9",
            "assign:5",
            "quiz:6",
            "board:7",
            "vod:8",
            "activity:page:8",
            "board-post:3:4",
            "resource:0123456789abcdef01234567",
            "representation:2",
            "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "assertion:7",
            "relation:8",
            "sync:9",
        ] {
            assert_eq!(value.parse::<LibraryRef>().unwrap().to_string(), value);
        }
    }
}
