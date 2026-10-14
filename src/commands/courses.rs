use super::Ctx;
use crate::{
    cli::{CourseListArgs, CoursesCommand},
    error::AppError,
    models::Course,
    output::{self, CommandResult, cell, listing, row, table},
    parse,
    reference::{ResourceRef, valid_id},
};

impl Ctx<'_> {
    pub(super) fn courses_command(
        &self,
        command: &CoursesCommand,
    ) -> Result<CommandResult, AppError> {
        let (client, base) = (self.client, self.base);
        match command {
            CoursesCommand::List(list) => self.course_listing("courses.list", None, list),
            CoursesCommand::Resolve { query, list } => {
                self.course_listing("courses.resolve", Some(query), list)
            }
            CoursesCommand::Show { course } => {
                let resolved = self.resolve_course(course)?;
                let response = client.get(&format!("/course/view.php?id={}", resolved.id))?;
                let model = parse::course_detail(&response.text, base, resolved)?;
                let professors = match model.professors.is_empty() {
                    true => "unknown".into(),
                    false => model.professors.join(", "),
                };
                let human = format!(
                    "{}\nID: {}\nCode: {}\nProfessors: {professors}\nActivities: {}\n{}",
                    model.course.title,
                    model.course.id,
                    cell(&model.course.code, "unknown"),
                    model.activity_count,
                    model.course.url
                );
                output::result("courses.show", &model, human)
            }
        }
    }
    pub(super) fn dashboard(&self, limit: usize) -> Result<CommandResult, AppError> {
        let mut model = parse::dashboard(&self.client.get("/my/")?.text, self.base)?;
        model.courses.truncate(limit);
        model.upcoming.truncate(limit);
        model.courses_complete = model.courses.len() == model.course_count;
        model.upcoming_complete = model.upcoming.len() == model.upcoming_count;
        let mut human = format!(
            "{} — {} courses, {} upcoming\n\nCourses:\nREF\tCODE\tTITLE",
            model.term.as_deref().unwrap_or("Current dashboard"),
            model.course_count,
            model.upcoming_count,
        );
        for course in &model.courses {
            human.push_str(&format!("\n{}", course_row(course)));
        }
        let (shown, total) = (model.courses.len(), model.course_count);
        if !model.courses_complete {
            human.push_str(&format!("\n[Showing {shown} of {total} courses]"));
        }
        human.push_str("\n\nUpcoming:");
        if model.upcoming.is_empty() {
            human.push_str("\nNone shown on the dashboard.");
        }
        for item in &model.upcoming {
            human.push_str(&format!("\n{}\t{}", item.title, item.url));
        }
        let (shown, total) = (model.upcoming.len(), model.upcoming_count);
        if shown > 0 && !model.upcoming_complete {
            human.push_str(&format!("\n[Showing {shown} of {total} upcoming items]"));
        }
        output::result("dashboard", &model, human)
    }

    fn courses(&self) -> Result<Vec<Course>, AppError> {
        Ok(parse::dashboard(&self.client.get("/my/")?.text, self.base)?.courses)
    }

    fn course_listing(
        &self,
        command: &'static str,
        query: Option<&str>,
        args: &CourseListArgs,
    ) -> Result<CommandResult, AppError> {
        let (courses, unknown_term) = self.scoped_courses(args)?;
        let courses = match query {
            Some(query) => matching_courses(courses, query),
            None => courses,
        };
        let mut result = listing(command, courses, args.list.limit, true, courses_table)?;
        if unknown_term > 0 {
            result.warnings.push(format!(
                "{unknown_term} course(s) were excluded because their term is unknown; use --all to see them"
            ));
        }
        Ok(result)
    }

    /// The scoped courses, and how many term filtering dropped for an unknown term.
    fn scoped_courses(&self, args: &CourseListArgs) -> Result<(Vec<Course>, usize), AppError> {
        if !args.all && args.year.is_none() {
            return Ok((self.courses()?, 0));
        }
        let key = parse::sesskey(&self.client.get("/my/")?.text)?;
        let mut courses = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut offset = 0;
        loop {
            let (page, next) = parse::enrolled_courses(
                &self.client.enrolled_courses(&key, offset, 100)?,
                self.base,
            )?;
            if page.is_empty() {
                break;
            }
            if next <= offset {
                return Err(AppError::shape(
                    "enrolled-course pagination did not advance",
                ));
            }
            for course in page {
                if seen.insert(course.id.clone()) {
                    courses.push(course);
                }
            }
            if next > 100_000 || courses.len() > 10_000 {
                return Err(AppError::limit(
                    "enrolled-course discovery exceeded its bounded pagination limit",
                ));
            }
            offset = next;
        }
        let mut unknown_term = 0;
        if let Some((year, semester)) = args.year.zip(args.semester) {
            let term = format!("{year}-{}", semester.course_suffix());
            unknown_term = courses.iter().filter(|c| c.term.is_none()).count();
            courses.retain(|course| course.term.as_deref() == Some(term.as_str()));
        }
        Ok((courses, unknown_term))
    }

    pub(super) fn resolve_course(&self, query: &str) -> Result<Course, AppError> {
        if query.starts_with("course:") {
            let ResourceRef::Course(id) = ResourceRef::parse(query)? else {
                unreachable!("course prefix parses only as a course")
            };
            return self.resolve_course(&id);
        }
        if valid_id(query) {
            let url = self.base.join(&format!("/course/view.php?id={query}"));
            return Ok(Course {
                id: query.into(),
                reference: format!("course:{query}"),
                title: format!("Course {query}"),
                code: None,
                term: None,
                url: url.expect("valid path").into(),
            });
        }
        let matches = matching_courses(self.courses()?, query);
        let exact: Vec<_> = matches.iter().filter(|c| is_exact(c, query)).collect();
        if let [course] = exact.as_slice() {
            return Ok((*course).clone());
        }
        match matches.as_slice() {
            [course] => Ok(course.clone()),
            [] => Err(AppError::not_found(format!(
                "no dashboard course matches {query:?}"
            ))),
            _ => {
                let candidates: Vec<_> = (matches.iter().take(5))
                    .map(|c| {
                        format!(
                            "{} ({})",
                            c.code.as_deref().unwrap_or(&c.reference),
                            c.title
                        )
                    })
                    .collect();
                Err(AppError::usage(format!(
                    "course query {query:?} is ambiguous: {}; use an exact code or course reference",
                    candidates.join(", ")
                )))
            }
        }
    }
}

fn is_exact(course: &Course, query: &str) -> bool {
    course.title.eq_ignore_ascii_case(query)
        || (course.code.as_ref()).is_some_and(|code| code.eq_ignore_ascii_case(query))
}

fn matching_courses(courses: Vec<Course>, query: &str) -> Vec<Course> {
    let needle = query.to_ascii_lowercase();
    let contains = |text: &str| text.to_ascii_lowercase().contains(&needle);
    let mut rows: Vec<_> = (courses.into_iter())
        .filter(|c| c.id == query || contains(&c.title) || c.code.as_deref().is_some_and(contains))
        .collect();
    rows.sort_by_key(|course| !(course.id == query || is_exact(course, query)));
    rows
}

fn course_row(course: &Course) -> String {
    row(&[&course.reference, cell(&course.code, "-"), &course.title])
}

fn courses_table(rows: &[Course], available: usize) -> String {
    table(rows, available, "Courses", "REF\tCODE\tTITLE", course_row)
}
