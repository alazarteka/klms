//! Endpoint policy for ordinary KLMS reads (the fixed AJAX methods are separate).
use crate::{
    error::AppError,
    reference::{ResourceRef, valid_id},
    url::{Url, query_value},
};

const MODULES: &[&str] = &[
    "assign",
    "quiz",
    "courseboard",
    "resource",
    "coursefile",
    "folder",
    "page",
    "url",
    "vod",
    "lti",
    "panopto",
    "panoptocourseembed",
];

fn query_only(url: &Url, keys: &[&str]) -> bool {
    let mut seen = std::collections::HashSet::new();
    url.query_pairs()
        .all(|(key, _)| keys.contains(&key.as_ref()) && seen.insert(key.into_owned()))
}

/// A pasted detail URL is an identity, never an arbitrary request target.
pub(crate) fn detail_ref(url: &Url) -> Option<ResourceRef> {
    let id = query_value(url, "id").filter(|id| valid_id(id))?;
    if url.path() == "/mod/courseboard/article.php" && query_only(url, &["id", "bwid"]) {
        return Some(ResourceRef::BoardPost {
            board: id,
            post: query_value(url, "bwid").filter(|id| valid_id(id))?,
        });
    }
    if !query_only(url, &["id"]) {
        return None;
    }
    let kind = url
        .path()
        .strip_prefix("/mod/")?
        .strip_suffix("/view.php")?;
    if !MODULES.contains(&kind) {
        return None;
    }
    if kind == "coursefile" {
        return Some(ResourceRef::Activity {
            kind: kind.into(),
            id,
        });
    }
    ResourceRef::from_url(url)
}

fn is_file(url: &Url) -> bool {
    (url.path().starts_with("/pluginfile.php/") && query_only(url, &["forcedownload"]))
        || detail_ref(url).is_some_and(|r| r.matches_module(&["resource", "coursefile", "folder"]))
}

pub(crate) fn read_url(url: &Url) -> Result<(), AppError> {
    let path = url.path();
    let keys: &[&str] = match path {
        "/my/" => &[],
        "/course/view.php" => &["id", "section"],
        "/calendar/view.php" => &["view", "time", "course"],
        "/grade/report/user/index.php"
        | "/local/lmsattendance/index.php"
        | "/mod/vod/viewer/index.php" => &["id"],
        "/mod/courseboard/article.php" => &["id", "bwid"],
        path if path.starts_with("/pluginfile.php/") => &["forcedownload"],
        path if path
            .strip_prefix("/mod/")
            .and_then(|rest| rest.split_once('/'))
            .is_some_and(|(kind, page)| {
                MODULES.contains(&kind) && matches!(page, "view.php" | "index.php")
            }) =>
        {
            if path == "/mod/courseboard/view.php" {
                &["id", "page"]
            } else {
                &["id"]
            }
        }
        _ => {
            return Err(AppError::usage(
                "request accepts known content-read endpoints only",
            ));
        }
    };
    if !query_only(url, keys) {
        return Err(AppError::usage(
            "request refuses unexpected, action, or secret query parameters",
        ));
    }
    Ok(())
}

pub(super) fn redirect(initial: &Url, next: &Url) -> Result<(), AppError> {
    // AJAX cannot redirect its method/body to a different operation.
    if initial.path() == "/lib/ajax/service.php" {
        return Err(AppError::network("AJAX redirect refused"));
    }
    read_url(next)?;
    if initial.path() == next.path()
        || (is_file(initial) && is_file(next))
        || (initial.path() == "/mod/panoptocourseembed/view.php"
            && next.path() == "/course/view.php")
    {
        Ok(())
    } else {
        Err(AppError::shape(
            "redirect changed the operation's endpoint or resource identity",
        ))
    }
}
