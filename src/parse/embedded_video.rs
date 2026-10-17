//! A Moodle embedded activity redirects to its course instead of a module page.
use super::{courses::activity_title, shared::sel};
use crate::{error::AppError, reference::valid_id};
use scraper::Html;

/// Isolate one activity before inspecting its content, preserving copied targets.
pub fn embedded_activity(html: &str, module: &str) -> Result<String, AppError> {
    if !valid_id(module) {
        return Err(AppError::usage(
            "embedded activity module id must be numeric",
        ));
    }
    let doc = Html::parse_document(html);
    let selector = sel(&format!("li#module-{module}, [data-id='{module}']"));
    let nodes: Vec<_> = doc.select(&selector).collect();
    match nodes.as_slice() {
        [node] => Ok(node.html()),
        _ => Err(AppError::shape(
            "course redirect must contain exactly one requested activity",
        )),
    }
}

/// The activity name inside an isolated fragment; the course page around it
/// carries the course's identity, not the module's.
pub fn embedded_title(fragment: &str) -> Option<String> {
    let doc = Html::parse_fragment(fragment);
    activity_title(doc.root_element())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn embedded_redirect_is_bound_to_exact_module_and_preserves_copied_target() {
        let html = r#"<li id="module-8" class="activity modtype_panoptocourseembed"><iframe src="/mod/panoptocourseembed/view_content.php?course=42&amp;contenturl=target"></iframe></li><li id="module-7" class="activity modtype_panoptocourseembed"><iframe src="/mod/panoptocourseembed/view_content.php?course=99&amp;contenturl=copied"></iframe></li>"#;
        let fragment = embedded_activity(html, "7").unwrap();
        assert!(fragment.contains("course=99") && !fragment.contains("course=42"));
        assert!(embedded_activity(html, "9").is_err());
        assert!(embedded_activity(&format!("{html}{html}"), "7").is_err());
        assert!(embedded_activity(html, "7']").is_err());
    }
}
