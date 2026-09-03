---
name: klms
description: Use the installed `klms` CLI for structured read-only KAIST KLMS access and its private versioned local course library, including bounded search, history, content, export, curation, and relationships. Prefer it over browser scraping and use its owned login flow.
---

# KLMS CLI

Use `klms --json` for machine-readable work. The global flag precedes the
command. `klms --json spec` returns the complete argument tree (paths, kinds,
required flags, choices, defaults, help) when you need to discover an option
rather than guess it:

```bash
klms --json doctor
klms auth login --method easy
klms auth login --method password --second-factor email
klms auth login --method password --second-factor sms
klms --json auth logout
klms --json auth time-left
klms --json dashboard
klms --json today
klms --json upcoming --through 7d
klms --json courses list
klms --json courses resolve QUERY
klms --json courses show COURSE
klms --json activities list --course COURSE --week 3 --limit 100
klms --json assignments list --course COURSE
klms --json quizzes list --course COURSE
klms --json calendar list
klms --json boards list --course COURSE
klms --json boards posts BOARD
klms --json notices list --course COURSE
klms --json files list --course COURSE
klms --json videos list --course COURSE
klms --json grades show --course COURSE
klms --json attendance show --course COURSE
klms --json library status
klms --json library sync --files
klms --json library sync --course COURSE --notices --files --download changed
klms --json library search QUERY --limit 50
klms --json library changes --limit 50
klms --json library activity --subject REF --limit 50
klms --json library show REF
klms --json library history REF --limit 50
klms --json library content REF --max-bytes 1048576
klms --json library export REF --out /absolute/new/path
```

Prefer the canonical `ref` returned by discovery and list commands. Titles and
codes are accepted for courses only when they resolve unambiguously. Read `ok`,
the process exit status, `warnings`, and collection `meta`. For library
queries, `complete` is local pagination completeness; `source_complete`
independently reports remote coverage and may be null when unknown. Do not
claim remote exhaustiveness unless `source_complete` is true. JSON schema
details live in the repository's `docs/JSON.md`.

The local library is the durable route for work across sessions. Sync only
when a human or agent decides current observations are needed; there is no
background schedule. Use `--files` to validate file metadata without
downloading and `--download changed` to store verified bytes once in the local
SHA-256 object store. An incomplete sync is evidence about that attempt, not
evidence that absent courses or resources were removed.
Likewise, an attachment becomes not-observed only after a complete typed detail
collection; a failed detail fetch preserves its prior state.
File validation is similarly scoped: HEAD observations do not rebind
stored bytes, downloads without a verified blob are unconditional, and a
course-scoped sync does not probe historical content outside its current
discovery frontier.

Inspect `library status` for the last attempt's scope/outcome and global
freshness, not just storage readiness. `last_sync.status: "unfinished"` means
completion was not recorded; the process may still be active or interrupted.
Check the original process before retrying the same command once it has stopped.
Do not infer liveness or rewrite history from this status. Partial syncs still
exit 0: inspect `data.status`, `failures`, `truncated`, and `warnings`.

Notice parsing excludes page controls, navigation, and view counters. The first
resync after upgrading older observations can record normalization changes;
preserve those historical records rather than deleting apparent duplicates.
No-longer-observed notice links are absent from current search, but their
history and curation remain accessible by reference.

Keep source and effective values distinct. To curate, first inspect the
subject's activity/history and pass the current field revision:

```bash
klms --json library edit REF --field title --value "Preferred title" --actor agent --expected-revision 0
klms --json library edit REF --field summary --value-file summary.md --actor agent --expected-revision 0
klms --json library relations add LEFT RIGHT --kind related_to --actor agent
klms --json library retract assertion:ID --actor agent
```

Humans and agents have equal authority. `actor` records provenance only. Never
retry `CURATION_CONFLICT` blindly: reread activity/history and decide whether
the new assertion should explicitly supersede the current revision. Summaries
are tied to an exact observation or blob; preserve `summary_stale` when source
content changes. Representation history contains both source-metadata and
verified-content events. Preserve every returned `sha256:` ref: it names an
exact historical byte version and can be passed directly to `library content`
or `library export`. Revisited bytes such as A→B→A remain three chronological
events while sharing one CAS object for repeated A bytes.

Relations are explicit assertions, not inferred candidates. A duplicate active
relation conflicts; retract its returned `relation:ID` before adding it again.

Treat `library show REF` as subject-specific. A representation reports only its
own effective filename/note/tag/summary, curation provenance, relationships,
and current SHA-256 ref; do not infer sibling state from its parent. Read stored
notice text with `library show REF` (`data.source.text`). `library content`
previews downloaded file bytes and `library export` exports those bytes; neither
downloads anything. For metadata-only files, follow the scoped download hint
only when downloading is within the user's request. Export refuses overwrites.
Non-file link representations are not undownloaded files: inspect their URL
with `library show REPRESENTATION_REF` (`data.source.url`), or read the parent
notice's stored text with `library show NOTICE_REF`. Do not repeat download
syncs to obtain link content or substitute a sibling attachment. If no file
candidates are recorded, or a file is marked not-observed, inspect the parent's
metadata and observation state; local absence does not establish remote absence.
If a resource has multiple stored attachments,
content/export returns `CONTENT_UNAVAILABLE` with candidate representation
refs. Select one explicitly rather than guessing.

For scheduled deadlines and calendar events, start with `today`, then
`upcoming --through 7d`. These commands do not claim to include unscheduled
notices, unread board posts, or work that KLMS omits from its calendar.
Use typed lists for exact course records:

```bash
klms --json assignments list --course course:180871 --limit 20
klms --json assignments show assign:1210516
klms --json notices list --course course:180871 --limit 20
klms --json notices show board-post:1189554:439261
klms --json files list --course course:180871 --limit 50
klms --json files download file:1205160 --out /absolute/output/path.pdf
```

Use `klms --json request get PATH --max-bytes N` only when a supported command
does not expose a needed same-origin read. It is a redacted, text-only,
experimental preview—not a lossless HTML or binary fetch. Prefer resource
commands because their output is typed and more stable. Do not follow Classum,
Panopto, Zoom, or other external links as if they shared KLMS authorization.

Remote operations are read-only except `klms auth extend`. Use that command
only when the user asks to extend the current session or when preserving an
active session is necessary for the task. It is safe to retry. `auth time-left`
reports the server value, but its first dashboard bootstrap may itself refresh
the timer; preserve the `bootstrap_may_have_extended_session` field when
explaining the result.

Do not reinterpret access as authorization to submit work, start quizzes,
check into attendance, or post messages. Downloads require an explicit `--out`
path and refuse overwrites.

Authentication is owned by `klms`. Inspect it without printing secrets using
`klms --json auth status`. If the result is `AUTH_REQUIRED`, ask the user to run
an interactive `klms auth login`; Easy Login and password login with email or
SMS verification are supported. `klms auth extend` cannot log in or revive an
expired session. Never display cookie values or ask the user to paste a
password or verification code into chat.
