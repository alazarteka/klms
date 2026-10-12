//! A Moodle embedded activity redirects to its course instead of a module page.
use crate::{error::AppError, reference::valid_id};
use scraper::{Html, Selector};

/// Isolate the exact module before inspecting its iframe, preserving copied targets.
pub fn embedded_activity(html: &str, module: &str) -> Result<String, AppError> {
    if !valid_id(module) {
        return Err(AppError::usage("embedded video module id must be numeric"));
    }
    let doc = Html::parse_document(html);
    let selector = Selector::parse(&format!("li#module-{module}.modtype_panoptocourseembed, .activity-item[data-id='{module}'].modtype_panoptocourseembed")).map_err(|_|AppError::internal("invalid numeric module selector"))?;
    let mut nodes = doc.select(&selector);
    let node = nodes.next().ok_or_else(|| {
        AppError::shape("embedded video redirect did not contain the requested module identity")
    })?;
    if nodes.next().is_some() {
        return Err(AppError::shape(
            "embedded video module identity was ambiguous",
        ));
    }
    Ok(node.html())
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
