# KLMS endpoint notes

These are the read surfaces used by the CLI. They are Moodle/KLMS implementation
details kept behind `client` and `parse`; command code should not contain HTML
selectors.

| Resource | Surface |
| --- | --- |
| Dashboard and course discovery | `/my/` |
| Course structure | `/course/view.php?id=COURSE` |
| Assignment index/detail | `/mod/assign/index.php?id=COURSE`, `/mod/assign/view.php?id=CM` |
| Quiz index/detail | `/mod/quiz/index.php?id=COURSE`, `/mod/quiz/view.php?id=CM` |
| Grades | `/grade/report/user/index.php?id=COURSE` |
| Attendance | `/local/lmsattendance/index.php?id=COURSE` |
| Calendar | `/calendar/view.php?view=upcoming` |
| Board posts/details | `/mod/courseboard/view.php?id=CM`, `/mod/courseboard/article.php?...` |
| Files | links discovered from course structure, `pluginfile.php` |
| VOD | links discovered from course structure, `/mod/vod/view.php?id=CM` |
| Session duration | Moodle AJAX methods `core_session_time_remaining`, `core_session_touch` |

Moodle AJAX calls use `/lib/ajax/service.php`, the authenticated page's
`sesskey`, a fixed allowlisted method name, and a JSON request body. `sesskey`
is never emitted. A protected local cache lets later timer checks avoid a
dashboard touch. If the cache is absent or stale, the command bootstraps from
an authenticated page and reports that this may itself have refreshed the
timer. A rejected cached key is discarded logically and retried through that
bootstrap path.

Classum, Panopto, Zoom, and arbitrary LTI destinations are different origins
and trust boundaries. Their links may be returned as metadata, but the KLMS
client neither follows nor authenticates to them.
