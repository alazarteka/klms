//! Fixed, paged folder enumeration; no unscoped query fallback.
use std::collections::HashSet;

use serde::Serialize;
use serde_json::Value;

use super::{Recordings, delivery::recording};
use crate::error::AppError;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FolderRequest<'a> {
    pub query_parameters: FolderParameters<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FolderParameters<'a> {
    #[serde(rename = "folderID")]
    pub folder_id: &'a str,
    pub max_results: u32,
    pub page: u32,
}

pub(super) fn collect(
    mut fetch: impl FnMut(u32) -> Result<Value, AppError>,
) -> Result<Recordings, AppError> {
    let mut rows = Vec::new();
    let mut ids = HashSet::new();
    let mut expected = 0_u64;
    for page in 0..100 {
        let value = fetch(page)?;
        let data = value
            .get("d")
            .ok_or_else(|| AppError::shape("Panopto folder response omitted d"))?;
        let results = data
            .get("Results")
            .and_then(Value::as_array)
            .ok_or_else(|| AppError::shape("Panopto folder response omitted Results"))?;
        if results.is_empty() {
            return Ok(Recordings {
                complete: rows.len() as u64 >= expected,
                rows,
            });
        }
        let total = data
            .get("TotalNumber")
            .and_then(Value::as_u64)
            .ok_or_else(|| AppError::shape("Panopto folder response omitted total count"))?;
        expected = expected.max(total);
        let mut added = false;
        for item in results {
            let id = item
                .get("DeliveryID")
                .and_then(Value::as_str)
                .filter(|id| crate::reference::valid_uuid(id))
                .ok_or_else(|| AppError::shape("Panopto folder row omitted recording UUID"))?;
            if ids.insert(id.to_ascii_lowercase()) {
                rows.push(recording(id, item));
                added = true;
            }
        }
        if !added {
            return Ok(Recordings {
                rows,
                complete: false,
            });
        }
    }
    Ok(Recordings {
        rows,
        complete: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn item(n: u32) -> Value {
        json!({"DeliveryID":format!("11111111-2222-3333-4444-{n:012}"),"SessionName":format!("Lecture {n}"),"Duration":30.0})
    }
    #[test]
    fn folder_request_uses_the_verified_lowercase_contract() {
        let body = FolderRequest {
            query_parameters: FolderParameters {
                folder_id: "11111111-2222-3333-4444-555555555555",
                max_results: 50,
                page: 2,
            },
        };
        assert_eq!(
            serde_json::to_value(body).unwrap(),
            json!({"queryParameters":{"folderID":"11111111-2222-3333-4444-555555555555","maxResults":50,"page":2}})
        );
    }
    #[test]
    fn pages_deduplicate_case_insensitively_and_require_exhaustion() {
        let mut calls = Vec::new();
        let result = collect(|page| {
            calls.push(page);
            Ok(match page {
                0 => json!({"d":{"Results":[item(1),item(2)],"TotalNumber":3}}),
                1 => json!({"d":{"Results":[item(2),item(3)],"TotalNumber":3}}),
                _ => json!({"d":{"Results":[],"TotalNumber":0}}),
            })
        })
        .unwrap();
        assert_eq!(calls, [0, 1, 2]);
        assert_eq!(result.rows.len(), 3);
        assert!(result.complete);
    }
    #[test]
    fn empty_folders_are_complete_but_stalls_missing_rows_and_failures_are_not() {
        assert!(
            collect(|_| Ok(json!({"d":{"Results":[],"TotalNumber":0}})))
                .unwrap()
                .complete
        );
        let stalled = collect(|_| Ok(json!({"d":{"Results":[item(1)],"TotalNumber":2}}))).unwrap();
        assert_eq!(stalled.rows.len(), 1);
        assert!(!stalled.complete);
        assert!(
            !collect(|page| Ok(if page == 0 {
                json!({"d":{"Results":[item(1)],"TotalNumber":2}})
            } else {
                json!({"d":{"Results":[],"TotalNumber":0}})
            }))
            .unwrap()
            .complete
        );
        assert!(collect(|_| Err(AppError::network("interrupted page"))).is_err());
        assert!(
            collect(|_| Ok(json!({"d":{"Results":[{"DeliveryID":"invalid"}],"TotalNumber":1}})))
                .is_err()
        );
    }
    #[test]
    fn endlessly_advancing_pages_hit_a_finite_incomplete_boundary() {
        let result =
            collect(|page| Ok(json!({"d":{"Results":[item(page)],"TotalNumber":101}}))).unwrap();
        assert_eq!(result.rows.len(), 100);
        assert!(!result.complete);
    }
}
