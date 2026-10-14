mod auth;
mod calendar;
mod courses;
mod coursework;
mod detail;
mod embedded_video;
mod native_video;
mod shared;
pub mod video;

pub(crate) use auth::login_markup;
pub use auth::{auth_handoff_form, auth_policy_shape, easy_login_code};
pub use calendar::calendar_page;
pub use courses::{
    activities, board_posts, course_detail, dashboard, enrolled_courses, has_all_weeks_view,
    is_notice_board,
};
pub use coursework::{assignments, attendance, grades, quizzes};
pub use detail::{has_next_page, next_page_url, resource_detail, safe_html_preview, sesskey};
pub use native_video::native_sources;

pub use embedded_video::{embedded_activity, embedded_title};
