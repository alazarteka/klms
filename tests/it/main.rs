//! Every integration test in one binary: cargo builds, links and runs one
//! test harness, and libtest runs all of its tests in parallel.

#[path = "../fixture/mod.rs"]
mod fixture;

mod archive_completeness;
mod auth_login;
mod cli_contract;
mod fixture_contract;
mod historical_courses;
mod installer_contract;
mod library_feedback;
mod parse_uncertainty;
mod policy_contract;
mod update_contract;
