use rusqlite::{TransactionBehavior, params};

use super::{
    Corpus, EditResult, LibraryRef, RelationResult, RetractionResult,
    query::{ACTIVE_RELATION, FIELDS, current_digest, effective_fields, refresh_subject, row},
};
use crate::{date::epoch_now as now, error::AppError};

fn require_actor(actor: &str) -> Result<(), AppError> {
    if actor.trim().is_empty() {
        return Err(AppError::usage("actor must not be empty"));
    }
    Ok(())
}

/// Parse a curation subject. Content-addressed blobs are immutable bytes, not
/// subjects with effective curation/search projections, so they are not editable.
fn editable_subject(value: &str) -> Result<LibraryRef, AppError> {
    let reference = value.parse::<LibraryRef>()?;
    match reference {
        LibraryRef::Course(_) | LibraryRef::Resource(_) | LibraryRef::Representation(_) => {
            Ok(reference)
        }
        _ => Err(AppError::usage(
            "curation subjects must be course, resource, or representation references",
        )),
    }
}

impl Corpus {
    pub fn edit(
        &mut self,
        subject: &str,
        field: &str,
        value: &str,
        actor: &str,
        expected_revision: u64,
    ) -> Result<EditResult, AppError> {
        if !FIELDS.contains(&field) {
            return Err(AppError::usage("invalid library field"));
        }
        if value.is_empty() || actor.trim().is_empty() {
            return Err(AppError::usage(
                "curation value and actor must not be empty",
            ));
        }
        let reference = editable_subject(subject)?;
        let subject = reference.to_string();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let digest = current_digest(&transaction, &reference)?
            .ok_or_else(|| AppError::not_found("library subject not found"))?;
        let mut current = effective_fields(&transaction, &subject)?;
        let revision = transaction.query_row(
            "SELECT COALESCE(MAX(revision),0) FROM assertions WHERE subject_ref=?1 AND field=?2",
            params![subject, field],
            |row| row.get::<_, i64>(0),
        )?;
        if revision as u64 != expected_revision {
            return Err(AppError::curation_conflict(format!(
                "expected revision {expected_revision}, current revision is {revision}"
            )));
        }
        let based_on = (field == "summary").then_some(digest);
        transaction.execute(
            "INSERT INTO assertions(subject_ref,field,value,actor,based_on,created_at,revision)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![subject, field, value, actor, based_on, now(), revision + 1],
        )?;
        let id = transaction.last_insert_rowid();
        refresh_subject(&transaction, &subject)?;
        transaction.commit()?;
        Ok(EditResult {
            reference: format!("assertion:{id}"),
            subject_ref: subject,
            field: field.into(),
            before: current.remove(field).map(|a| a.value),
            after: value.into(),
            revision: revision + 1,
            actor: actor.into(),
        })
    }

    pub fn retract(&mut self, target: &str, actor: &str) -> Result<RetractionResult, AppError> {
        require_actor(actor)?;
        let parsed = target.parse::<LibraryRef>()?;
        let (sql, id) = match parsed {
            LibraryRef::Assertion(id) => ("SELECT subject_ref FROM assertions WHERE id=?1", id),
            LibraryRef::Relation(id) => ("SELECT left_ref FROM relations WHERE id=?1", id),
            _ => {
                return Err(AppError::usage(
                    "retract accepts an assertion or relation reference",
                ));
            }
        };
        let target = parsed.to_string();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let subject = row(&transaction, sql, [id], |r| r.get::<_, String>(0))?
            .ok_or_else(|| AppError::not_found("curation target not found"))?;
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO retractions(target_ref,actor,created_at) VALUES(?1,?2,?3)",
            params![target, actor, now()],
        )?;
        if inserted == 0 {
            return Err(AppError::curation_conflict("target is already retracted"));
        }
        if matches!(parsed, LibraryRef::Assertion(_)) {
            refresh_subject(&transaction, &subject)?;
        }
        transaction.commit()?;
        Ok(RetractionResult {
            reference: target.clone(),
            target_ref: target,
            actor: actor.into(),
        })
    }

    pub fn add_relation(
        &mut self,
        left: &str,
        right: &str,
        kind: &str,
        actor: &str,
    ) -> Result<RelationResult, AppError> {
        if !matches!(
            kind,
            "revision_of" | "duplicate_of" | "derived_from" | "related_to"
        ) {
            return Err(AppError::usage("invalid relation kind"));
        }
        let (left_ref, right_ref) = (editable_subject(left)?, editable_subject(right)?);
        require_actor(actor)?;
        let (left, right) = (left_ref.to_string(), right_ref.to_string());
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if current_digest(&transaction, &left_ref)?.is_none()
            || current_digest(&transaction, &right_ref)?.is_none()
        {
            return Err(AppError::not_found("relation endpoint not found"));
        }
        let duplicate = transaction.query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM relations r
                  WHERE r.left_ref=?1 AND r.right_ref=?2 AND r.kind=?3 AND {ACTIVE_RELATION})"
            ),
            params![left, right, kind],
            |row| row.get::<_, bool>(0),
        )?;
        if duplicate {
            return Err(AppError::curation_conflict(
                "active relation already exists",
            ));
        }
        transaction.execute(
            "INSERT INTO relations(left_ref,right_ref,kind,actor,created_at)
             VALUES(?1,?2,?3,?4,?5)",
            params![left, right, kind, actor, now()],
        )?;
        let id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(RelationResult {
            reference: format!("relation:{id}"),
        })
    }
}
