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

Moodle AJAX calls use `/lib/ajax/service.php`, the `sesskey` read from a fresh
`/my/` response, a fixed allowlisted method name, and a JSON request body. The
`sesskey` is held in memory only and never emitted or persisted, so every timer
check bootstraps from `/my/`, which may itself refresh the timer.

Classum, Panopto, Zoom, and arbitrary LTI destinations are different origins
and trust boundaries. Their links may be returned as metadata, but the KLMS
client neither follows nor authenticates to them.
