use std::fmt;

use crate::{
    error::AppError,
    url::{Url, query_value},
};

const VIDEO_KINDS: [&str; 4] = ["vod", "lti", "panopto", "panoptocourseembed"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceRef {
    Course(String),
    PanoptoRecording {
        kind: String,
        module: String,
        uuid: String,
    },
    Assignment(String),
    Quiz(String),
    Board(String),
    BoardPost {
        board: String,
        post: String,
    },
    File(String),
    Activity {
        kind: String,
        id: String,
    },
    Video {
        kind: String,
        id: String,
    },
}

impl ResourceRef {
    pub fn parse(value: &str) -> Result<Self, AppError> {
        let parts: Vec<_> = value.split(':').collect();
        let id = |s: &str| s.to_owned();
        match parts.as_slice() {
            ["course", i] if valid_id(i) => Ok(Self::Course(id(i))),
            ["panopto-recording", kind, module, uuid]
                if ["lti", "panopto", "panoptocourseembed"].contains(kind)
                    && valid_id(module)
                    && valid_uuid(uuid) =>
            {
                Ok(Self::PanoptoRecording {
                    kind: id(kind),
                    module: id(module),
                    uuid: uuid.to_ascii_lowercase(),
                })
            }
            ["assign", i] if valid_id(i) => Ok(Self::Assignment(id(i))),
            ["quiz", i] if valid_id(i) => Ok(Self::Quiz(id(i))),
            ["board", i] if valid_id(i) => Ok(Self::Board(id(i))),
            ["board-post", b, p] if valid_id(b) && valid_id(p) => Ok(Self::BoardPost {
                board: id(b),
                post: id(p),
            }),
            ["file", i] if valid_id(i) => Ok(Self::File(id(i))),
            ["activity", k, i] if valid_id(i) && valid_kind(k) => Ok(Self::Activity {
                kind: id(k),
                id: id(i),
            }),
            [k, i] if VIDEO_KINDS.contains(k) && valid_id(i) => Ok(Self::Video {
                kind: id(k),
                id: id(i),
            }),
            _ => Err(AppError::usage(format!(
                "invalid resource reference {value:?}"
            ))),
        }
    }

    pub fn from_activity(kind: &str, id: Option<&str>, url: Option<&Url>) -> Option<Self> {
        let id = id.map(str::to_owned).or_else(|| query_value(url?, "id"))?;
        let kind = kind.to_ascii_lowercase();
        if !valid_id(&id) {
            return None;
        }
        match kind.as_str() {
            "assign" => Some(Self::Assignment(id)),
            "quiz" => Some(Self::Quiz(id)),
            "courseboard" => Some(Self::Board(id)),
            "resource" | "coursefile" => Some(Self::File(id)),
            k if VIDEO_KINDS.contains(&k) => Some(Self::Video { kind, id }),
            k if valid_kind(k) => Some(Self::Activity { kind, id }),
            _ => None,
        }
    }

    pub fn from_url(url: &Url) -> Option<Self> {
        Self::from_activity(module_kind(url)?, None, Some(url))
    }

    pub fn activity_kind(&self) -> Option<&str> {
        match self {
            Self::Assignment(_) => Some("assign"),
            Self::Quiz(_) => Some("quiz"),
            Self::Board(_) => Some("courseboard"),
            Self::File(_) => Some("resource"),
            Self::Activity { kind, .. } | Self::Video { kind, .. } => Some(kind),
            Self::Course(_) | Self::BoardPost { .. } | Self::PanoptoRecording { .. } => None,
        }
    }

    pub fn path(&self) -> String {
        match self {
            Self::PanoptoRecording { kind, module, .. } => {
                format!("/mod/{kind}/view.php?id={module}")
            }
            Self::Course(id) => format!("/course/view.php?id={id}"),
            Self::BoardPost { board, post } => {
                format!("/mod/courseboard/article.php?id={board}&bwid={post}")
            }
            Self::Assignment(id) | Self::Quiz(id) | Self::Board(id) | Self::File(id) => {
                let kind = self.activity_kind().unwrap_or_default();
                format!("/mod/{kind}/view.php?id={id}")
            }
            Self::Activity { kind, id } | Self::Video { kind, id } => {
                format!("/mod/{kind}/view.php?id={id}")
            }
        }
    }

    pub fn matches_module(&self, kinds: &[&str]) -> bool {
        match self {
            Self::File(_) => kinds
                .iter()
                .any(|kind| matches!(*kind, "resource" | "coursefile")),
            _ => self
                .activity_kind()
                .is_some_and(|kind| kinds.contains(&kind)),
        }
    }
}

impl fmt::Display for ResourceRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PanoptoRecording { kind, module, uuid } => {
                write!(formatter, "panopto-recording:{kind}:{module}:{uuid}")
            }
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

/// The segment after `/mod/` in a Moodle activity path.
pub(crate) fn module_kind(url: &Url) -> Option<&str> {
    let mut segments = url.path_segments()?.skip_while(|part| *part != "mod");
    segments.nth(1)
}

pub(crate) fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn valid_kind(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::ResourceRef;
    use crate::url::Url;

    #[test]
    fn module_kind_reads_moodle_paths() {
        for (path, expected) in [
            ("/mod/quiz/view.php?id=7", Some("quiz")),
            ("/mod", None),
            ("/course/view.php?id=7", None),
        ] {
            let url = Url::parse(&format!("https://a.test{path}")).unwrap();
            assert_eq!(super::module_kind(&url), expected);
        }
    }

    #[test]
    fn canonical_refs_round_trip_to_paths() {
        let reference = ResourceRef::parse("board-post:12:34").unwrap();
        assert_eq!(reference.to_string(), "board-post:12:34");
        assert_eq!(
            reference.path(),
            "/mod/courseboard/article.php?id=12&bwid=34"
        );
        assert_eq!(
            ResourceRef::parse("quiz:7").unwrap().path(),
            "/mod/quiz/view.php?id=7"
        );
        assert!(ResourceRef::parse("123").is_err());
        assert!(ResourceRef::parse("assign:not-a-number").is_err());
    }

    #[test]
    fn inferred_references_obey_the_same_identity_rules_as_cli_input() {
        for id in ["", "oops", "-1", "1:2", "１２"] {
            assert!(ResourceRef::from_activity("assign", Some(id), None).is_none());
            let url = Url::parse(&format!(
                "https://klms.kaist.ac.kr/mod/assign/view.php?id={id}"
            ))
            .unwrap();
            assert!(ResourceRef::from_activity("assign", None, Some(&url)).is_none());
            assert!(ResourceRef::from_url(&url).is_none());
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
}

fn valid_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

#[cfg(test)]
mod video_tests {
    use super::*;
    #[test]
    fn recording_refs_retain_launch_provenance_and_reject_ambiguous_identity() {
        let value = "panopto-recording:panoptocourseembed:7:11111111-2222-3333-4444-555555555555";
        let r = ResourceRef::parse(value).unwrap();
        assert_eq!(r.to_string(), value);
        assert_eq!(r.path(), "/mod/panoptocourseembed/view.php?id=7");
        for bad in [
            "panopto-recording:11111111-2222-3333-4444-555555555555",
            "panopto-recording:vod:7:11111111-2222-3333-4444-555555555555",
            "panopto-recording:lti:7:not-a-uuid",
        ] {
            assert!(ResourceRef::parse(bad).is_err());
        }
    }
}
