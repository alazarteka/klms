use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    Corpus, EditResult, LibraryRef, RelationResult, RetractionResult,
    query::{ACTIVE_RELATION, current_digest, effective_field, refresh_subject},
    storage::now,
};
use crate::error::AppError;

impl Corpus {
    pub fn edit(
        &mut self,
        subject: &str,
        field: &str,
        value: &str,
        actor: &str,
        expected_revision: u64,
    ) -> Result<EditResult, AppError> {
        if !matches!(field, "title" | "filename" | "summary" | "note" | "tag") {
            return Err(AppError::usage("invalid library field"));
        }
        if value.is_empty() || actor.trim().is_empty() {
            return Err(AppError::usage(
                "curation value and actor must not be empty",
            ));
        }
        let reference = subject.parse::<LibraryRef>()?;
        let transaction = self
            .storage
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let digest = current_digest(&transaction, &reference)?
            .ok_or_else(|| AppError::not_found("library subject not found"))?;
        let revision = transaction.query_row(
            "SELECT COALESCE(MAX(revision),0) FROM assertions
              WHERE subject_ref=?1 AND field=?2",
            params![subject, field],
            |row| row.get::<_, i64>(0),
        )?;
        if revision as u64 != expected_revision {
            return Err(AppError::curation_conflict(format!(
                "expected revision {expected_revision}, current revision is {revision}"
            )));
        }
        let before = effective_field(&transaction, subject, field)?.map(|row| row.value);
        let based_on = (field == "summary").then_some(digest);
        transaction.execute(
            "INSERT INTO assertions(
               subject_ref,field,value,actor,based_on,created_at,revision
             ) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![subject, field, value, actor, based_on, now(), revision + 1],
        )?;
        let id = transaction.last_insert_rowid();
        refresh_subject(&transaction, subject)?;
        transaction.commit()?;
        Ok(EditResult {
            reference: format!("assertion:{id}"),
            subject_ref: subject.into(),
            field: field.into(),
            before,
            after: value.into(),
            revision: revision + 1,
            actor: actor.into(),
        })
    }

    pub fn retract(&mut self, target: &str, actor: &str) -> Result<RetractionResult, AppError> {
        if actor.trim().is_empty() {
            return Err(AppError::usage("actor must not be empty"));
        }
        let parsed = target.parse::<LibraryRef>()?;
        if !matches!(parsed, LibraryRef::Assertion(_) | LibraryRef::Relation(_)) {
            return Err(AppError::usage(
                "retract accepts an assertion or relation reference",
            ));
        }
        let transaction = self
            .storage
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let subject = match parsed {
            LibraryRef::Assertion(id) => transaction
                .query_row(
                    "SELECT subject_ref FROM assertions WHERE id=?1",
                    [id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?,
            LibraryRef::Relation(id) => transaction
                .query_row("SELECT left_ref FROM relations WHERE id=?1", [id], |row| {
                    row.get::<_, String>(0)
                })
                .optional()?,
            _ => None,
        }
        .ok_or_else(|| AppError::not_found("curation target not found"))?;
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO retractions(target_ref,actor,created_at)
             VALUES(?1,?2,?3)",
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
            reference: target.into(),
            target_ref: target.into(),
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
        let left_ref = left.parse::<LibraryRef>()?;
        let right_ref = right.parse::<LibraryRef>()?;
        if actor.trim().is_empty() {
            return Err(AppError::usage("actor must not be empty"));
        }
        let transaction = self
            .storage
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if current_digest(&transaction, &left_ref)?.is_none()
            || current_digest(&transaction, &right_ref)?.is_none()
        {
            return Err(AppError::not_found("relation endpoint not found"));
        }
        let sql = format!(
            "SELECT EXISTS(SELECT 1 FROM relations r
              WHERE r.left_ref=?1 AND r.right_ref=?2 AND r.kind=?3
                AND {ACTIVE_RELATION})"
        );
        let duplicate = transaction
            .query_row(&sql, params![left, right, kind], |row| row.get::<_, i64>(0))?
            != 0;
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
