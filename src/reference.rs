use std::fmt;

use url::Url;

use crate::error::AppError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceRef {
    Course(String),
    Assignment(String),
    Quiz(String),
    Board(String),
    BoardPost { board: String, post: String },
    File(String),
    Activity { kind: String, id: String },
    Video { kind: String, id: String },
}

impl ResourceRef {
    pub fn parse(value: &str) -> Result<Self, AppError> {
        let parts: Vec<_> = value.split(':').collect();
        match parts.as_slice() {
            ["course", id] if valid_id(id) => Ok(Self::Course((*id).into())),
            ["assign", id] if valid_id(id) => Ok(Self::Assignment((*id).into())),
            ["quiz", id] if valid_id(id) => Ok(Self::Quiz((*id).into())),
            ["board", id] if valid_id(id) => Ok(Self::Board((*id).into())),
            ["board-post", board, post] if valid_id(board) && valid_id(post) => {
                Ok(Self::BoardPost {
                    board: (*board).into(),
                    post: (*post).into(),
                })
            }
            ["file", id] if valid_id(id) => Ok(Self::File((*id).into())),
            ["activity", kind, id]
                if valid_id(id)
                    && !kind.is_empty()
                    && kind
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') =>
            {
                Ok(Self::Activity {
                    kind: (*kind).into(),
                    id: (*id).into(),
                })
            }
            [
                kind @ ("vod" | "lti" | "panopto" | "panoptocourseembed"),
                id,
            ] if valid_id(id) => Ok(Self::Video {
                kind: (*kind).into(),
                id: (*id).into(),
            }),
            _ => Err(AppError::usage(format!(
                "invalid resource reference {value:?}"
            ))),
        }
    }

    pub fn from_activity(kind: &str, id: Option<&str>, url: Option<&str>) -> Option<Self> {
        let id = id.map(str::to_owned).or_else(|| url.and_then(module_id))?;
        if !valid_id(&id) {
            return None;
        }
        match kind.to_ascii_lowercase().as_str() {
            "assign" => Some(Self::Assignment(id)),
            "quiz" => Some(Self::Quiz(id)),
            "courseboard" => Some(Self::Board(id)),
            "resource" | "coursefile" => Some(Self::File(id)),
            "vod" | "lti" | "panopto" | "panoptocourseembed" => Some(Self::Video {
                kind: kind.to_ascii_lowercase(),
                id,
            }),
            other
                if !other.is_empty()
                    && other
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') =>
            {
                Some(Self::Activity {
                    kind: other.into(),
                    id,
                })
            }
            _ => None,
        }
    }

    pub fn from_url(url: &Url) -> Option<Self> {
        let parts: Vec<_> = url.path_segments()?.collect();
        let kind = parts
            .windows(2)
            .find_map(|pair| (pair[0] == "mod").then_some(pair[1]))?;
        Self::from_activity(kind, None, Some(url.as_str()))
    }

    pub fn activity_kind(&self) -> Option<&str> {
        match self {
            Self::Assignment(_) => Some("assign"),
            Self::Quiz(_) => Some("quiz"),
            Self::Board(_) => Some("courseboard"),
            Self::File(_) => Some("resource"),
            Self::Activity { kind, .. } => Some(kind),
            Self::Video { kind, .. } => Some(kind),
            Self::Course(_) | Self::BoardPost { .. } => None,
        }
    }

    pub fn path(&self) -> String {
        match self {
            Self::Course(id) => format!("/course/view.php?id={id}"),
            Self::Assignment(id) => format!("/mod/assign/view.php?id={id}"),
            Self::Quiz(id) => format!("/mod/quiz/view.php?id={id}"),
            Self::Board(id) => format!("/mod/courseboard/view.php?id={id}"),
            Self::BoardPost { board, post } => {
                format!("/mod/courseboard/article.php?id={board}&bwid={post}")
            }
            Self::File(id) => format!("/mod/resource/view.php?id={id}"),
            Self::Activity { kind, id } => format!("/mod/{kind}/view.php?id={id}"),
            Self::Video { kind, id } => format!("/mod/{kind}/view.php?id={id}"),
        }
    }

    pub fn matches_module(&self, kinds: &[&str]) -> bool {
        match self {
            Self::Assignment(_) => kinds.contains(&"assign"),
            Self::Quiz(_) => kinds.contains(&"quiz"),
            Self::Board(_) => kinds.contains(&"courseboard"),
            Self::File(_) => kinds
                .iter()
                .any(|kind| matches!(*kind, "resource" | "coursefile")),
            Self::Activity { kind, .. } => kinds.iter().any(|expected| kind == expected),
            Self::Video { kind, .. } => kinds.iter().any(|expected| kind == expected),
            Self::Course(_) | Self::BoardPost { .. } => false,
        }
    }
}

impl fmt::Display for ResourceRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Course(id) => write!(formatter, "course:{id}"),
            Self::Assignment(id) => write!(formatter, "assign:{id}"),
            Self::Quiz(id) => write!(formatter, "quiz:{id}"),
            Self::Board(id) => write!(formatter, "board:{id}"),
            Self::BoardPost { board, post } => write!(formatter, "board-post:{board}:{post}"),
            Self::File(id) => write!(formatter, "file:{id}"),
            Self::Activity { kind, id } => write!(formatter, "activity:{kind}:{id}"),
            Self::Video { kind, id } => write!(formatter, "{kind}:{id}"),
        }
    }
}

pub(crate) fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn module_id(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    url.query_pairs()
        .find_map(|(key, value)| (key == "id").then(|| value.into_owned()))
        .filter(|value| valid_id(value))
}

#[cfg(test)]
mod tests {
    use super::ResourceRef;
    use url::Url;

    #[test]
    fn canonical_refs_round_trip_to_paths() {
        let reference = ResourceRef::parse("board-post:12:34").unwrap();
        assert_eq!(reference.to_string(), "board-post:12:34");
        assert_eq!(
            reference.path(),
            "/mod/courseboard/article.php?id=12&bwid=34"
        );
    }

    #[test]
    fn inferred_references_obey_the_same_identity_rules_as_cli_input() {
        for id in ["", "oops", "-1", "1:2", "１２"] {
            assert!(ResourceRef::from_activity("assign", Some(id), None).is_none());
            let url = format!("https://klms.kaist.ac.kr/mod/assign/view.php?id={id}");
            assert!(ResourceRef::from_activity("assign", None, Some(&url)).is_none());
            assert!(ResourceRef::from_url(&Url::parse(&url).unwrap()).is_none());
        }
        assert!(ResourceRef::from_activity("", Some("7"), None).is_none());
        for kind in ["assign", "quiz", "courseboard", "resource", "vod", "CUSTOM"] {
            let reference = ResourceRef::from_activity(kind, Some("007"), None).unwrap();
            assert_eq!(
                ResourceRef::parse(&reference.to_string()).unwrap(),
                reference
            );
        }
    }

    #[test]
    fn rejects_untyped_or_non_numeric_refs() {
        assert!(ResourceRef::parse("123").is_err());
        assert!(ResourceRef::parse("assign:not-a-number").is_err());
    }
}
