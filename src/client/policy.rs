//! Endpoint policy for ordinary KLMS reads (the fixed AJAX methods are separate).
use crate::{
    error::AppError,
    reference::{ResourceRef, valid_id},
    url::{Url, query_value},
};

/// Query keys Moodle's read-only pluginfile.php understands; anything else
/// (e.g. a sesskey) is refused.
const PLUGINFILE: &[&str] = &[
    "file",
    "forcedownload",
    "preview",
    "offline",
    "embed",
    "time",
];

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

pub(crate) fn pluginfile(url: &Url) -> bool {
    url.path().starts_with("/pluginfile.php/")
        || (url.path() == "/pluginfile.php"
            && query_value(url, "file").is_some_and(|file| file.starts_with('/') && file.len() > 1))
}

pub(crate) fn read_url(url: &Url) -> Result<(), AppError> {
    let path = url.path();
    let keys: &[&str] = match path {
        "/my/" => &[],
        "/course/view.php" => &["id", "section", "sectionid"],
        "/calendar/view.php" => &[
            "view",
            "time",
            "course",
            "lookahead",
            "category",
            "cal_d",
            "cal_m",
            "cal_y",
        ],
        "/grade/report/user/index.php"
        | "/local/lmsattendance/index.php"
        | "/mod/vod/viewer/index.php" => &["id"],
        "/mod/courseboard/article.php" => &["id", "bwid"],
        _ if pluginfile(url) => PLUGINFILE,
        path if path
            .strip_prefix("/mod/")
            .and_then(|rest| rest.split_once('/'))
            .is_some_and(|(kind, page)| {
                MODULES.contains(&kind) && matches!(page, "view.php" | "index.php")
            }) =>
        {
            if path == "/mod/courseboard/view.php" {
                &["id", "page", "redirect", "forceview"]
            } else if path.ends_with("/view.php") {
                &["id", "redirect", "forceview"]
            } else {
                &["id"]
            }
        }
        _ => {
            return Err(AppError::shape(
                "request accepts known content-read endpoints only",
            ));
        }
    };
    if !query_only(url, keys) {
        return Err(AppError::shape(
            "request refuses unexpected, action, or secret query parameters",
        ));
    }
    Ok(())
}

pub(super) fn redirect(initial: &Url, next: &Url) -> Result<(), AppError> {
    // AJAX cannot redirect its method/body to a different operation.
    if initial.path() == "/lib/ajax/service.php" {
        return Err(AppError::shape("AJAX redirect refused"));
    }
    // KLMS answers an expired session by redirecting to its login page.
    if next.path().to_ascii_lowercase().contains("/login/") {
        return Err(AppError::auth_required(
            "the saved KLMS session is missing or expired",
        ));
    }
    // Moodle sends a course or activity the account cannot open to the
    // course's enrolment page; that is an access answer, not a markup change.
    if next.path() == "/enrol/index.php" {
        return Err(AppError::http(403, initial.path()));
    }
    read_url(next)?;
    let same_id = |key| match (query_value(initial, key), query_value(next, key)) {
        (Some(a), Some(b)) => {
            valid_id(&a) && valid_id(&b) && a.trim_start_matches('0') == b.trim_start_matches('0')
        }
        (None, None) => true,
        _ => false,
    };
    let module = initial
        .path()
        .strip_prefix("/mod/")
        .and_then(|path| path.strip_suffix("/view.php"));
    let allowed = match module {
        Some("url") => true,
        Some("resource" | "coursefile") if pluginfile(next) => true,
        Some("folder" | "panoptocourseembed") if next.path() == "/course/view.php" => {
            query_value(next, "id").is_some_and(|id| valid_id(&id))
        }
        _ if pluginfile(initial) => pluginfile(next),
        _ => initial.path() == next.path() && same_id("id") && same_id("bwid"),
    };
    if allowed {
        Ok(())
    } else {
        Err(AppError::shape(
            "redirect changed the operation's endpoint or resource identity",
        ))
    }
}
