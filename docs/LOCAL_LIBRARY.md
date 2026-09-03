# Versioned local library

The library is a private local record of what KLMS showed the user plus a
curation layer that humans and agents use with equal authority. Synchronizing
is finite, typed, and read-only toward KLMS.

## Invariants

1. Source observations and content blobs are immutable.
2. Sync never overwrites curation.
3. Actor is provenance, not authority.
4. Sequential edits supersede; stale expected revisions conflict.
5. Absence is recorded only after a complete collection observation.
6. A missing course means only that it is not currently listed.
7. Failed or incomplete syncs create no missing event.
8. Remote changes and curation activity are separate streams.
9. Local SHA-256 identifies bytes; ETags are opaque validators.
10. Summaries are bound to a source digest and report staleness.
11. External links never receive KLMS credentials.
12. Raw HTML, cookies, and secret headers are never persisted.
13. Collections are bounded and report truncation and source coverage.

## Layout and privacy

SQLite lives at `$XDG_DATA_HOME/klms/library.db`, falling back to
`~/.local/share/klms/library.db`. Content-addressed objects live under the
adjacent `objects/sha256` directory. Directories use mode 0700 and files use
0600 on Unix. `corpus` alone owns SQLite and the object store; commands contain
no SQL. Network policy remains in `client`, and KLMS selectors remain in
`parse`.

Back up `library.db` and `objects` together while no library command is
running. Preserve a corrupt library before attempting recovery.

## References

- `course:ID`: upstream course
- `file:ID`, `activity:KIND:ID`, `board-post:BOARD:POST`: upstream resource
- `resource:HASH`: resource without an upstream reference
- `representation:N`: one locator of a resource
- `sha256:HEX`: exact stored bytes
- `assertion:N`, `relation:N`, `sync:N`: local records

## Commands

```text
klms library status
klms library sync [--course COURSE] [--notices] [--files] [--download changed]
klms library search QUERY [--limit N]
klms library changes [--limit N]
klms library activity [--subject REF] [--limit N]
klms library show REF
klms library history REF [--limit N]
klms library content REF [--max-bytes N]
klms library export REF --out PATH
klms library edit REF --field FIELD (--value TEXT | --value-file PATH)
                  [--actor ACTOR] --expected-revision N
klms library retract REF [--actor ACTOR]
klms library relations add LEFT RIGHT --kind KIND [--actor ACTOR]
```

Only `sync` loads a session. With no flags, sync records courses, manifests,
and typed activity details. `--notices` walks bounded board pagination.
`--files` validates file representations with HEAD. `--download changed`
implies `--files`, conditionally fetches changed bytes, and deduplicates them
by SHA-256. Parser caps produce incomplete observations without failing a run
and are counted in the sync summary's `truncated` field; fetch or parse
failures make the run incomplete.

Notice observations contain the post title, body, body links, and attachments,
not surrounding navigation, dialogs, or view counters. Unrecognized notice
structure fails visibly rather than storing the whole page. After upgrading
from broad page extraction, the first notice resync can record normalization
changes; old observations and change events are not rewritten. Obsolete notice
links stop appearing in current search but remain available by reference with
their history and curation. Historical downloaded files remain searchable.

Two coverage limits are worth knowing. The dashboard parser has no way to
detect a truncated course list, so a successful global sync always treats the
course list as complete; a failed dashboard fetch marks nothing missing.
Notices are observed only when `--notices` is given and are never marked
missing, because board pagination is bounded rather than exhaustive.

`content` and `export` operate only on downloaded file bytes; neither command
downloads anything. Read stored notice text with `klms library show REF`
(`--json`: `data.source.text`). For metadata-only files, explicitly run
`klms library sync --course COURSE --download changed` to store bytes.
Include `--notices` for notice attachments.
When a notice has no downloaded bytes, its error lists up to 20 currently
recorded file attachment references and names alongside the text-reading
option, even when the stored text is just punctuation. After an explicit
download sync, use an attachment reference with `content` or `export`.

Non-file links are stored metadata, not undownloaded files. Inspect a link's
URL with `klms library show REPRESENTATION_REF` (`data.source.url`); read a
parent notice's text with `klms library show NOTICE_REF`. Content/export never
follow the link or substitute a sibling attachment. Download hints apply only
to recorded file candidates still marked present, not links. For a missing
file or a resource without recorded file candidates, inspect its stored
metadata and observation state; local absence does not prove remote absence
or guarantee that another sync will recover bytes.

`content` returns a bounded preview and reports `truncated` inside its data
object. `export` verifies the digest and refuses an existing destination.
Multiple downloaded representations produce `CONTENT_UNAVAILABLE` with
candidate references and a selection hint; choose a representation or exact
`sha256:` value. Missing content errors explain the appropriate recovery path.

`edit --field summary` binds the assertion to the current source digest.
Retraction never deletes its assertion or relation. Effective fields use the
highest active revision and include assertion provenance.

The JSON envelope is schema `"4"`; the SQLite schema uses `user_version = 1`.
Local collection `complete` means the local limit did not truncate results.
`source_complete` describes the latest global sync, while `fresh_through` is
the finish time of the latest complete, source-complete global sync.

Human status separates storage readiness from the last sync attempt, scope,
times, and last complete global sync. Course-scoped success does not establish
global coverage. Local lists warn when truncated and report empty results;
warnings appear on stderr in human mode and in the JSON envelope otherwise.
Partial syncs retain exit status 0 but explicitly report `incomplete` and their
failures; callers must inspect the result rather than treating exit 0 as proof
of complete source coverage.

An attempt without a completion record is exposed as `unfinished`, whether its
process is still active or was interrupted. Status does not track process
liveness. Check the original process and retry the same command once it has
stopped. This projection does not rewrite persisted `running` rows, fabricate
finish times, or change the last known complete global-sync time.

## Errors and validation boundary

Library-specific errors are `MIGRATION_REQUIRED`, `CORPUS_BUSY`,
`CURATION_CONFLICT`, `CONTENT_UNAVAILABLE`, `CORPUS_CORRUPT`, and
`LIBRARY_IO`. Validation covers synthetic integration cases and sampled live
notice sync and file downloads, not every course or file type.
Committed fixtures contain no real KLMS HTML, credentials, grades, attendance,
or course data.
