pub const VERSION: u32 = 1;
pub const SCHEMA: &str = r#"
CREATE TABLE sync_runs (
  id INTEGER PRIMARY KEY,
  started_at INTEGER NOT NULL,
  finished_at INTEGER,
  scope TEXT NOT NULL,
  status TEXT NOT NULL
    CHECK (status IN ('running','complete','incomplete','failed')),
  source_complete INTEGER NOT NULL DEFAULT 0,
  failures TEXT NOT NULL DEFAULT '[]'
) STRICT;
CREATE TABLE courses (
  id INTEGER PRIMARY KEY,
  ref TEXT NOT NULL UNIQUE,
  remote_state TEXT NOT NULL DEFAULT 'listed'
    CHECK (remote_state IN ('listed','not_listed')),
  first_seen INTEGER NOT NULL,
  last_seen INTEGER NOT NULL,
  not_listed_since INTEGER
) STRICT;

CREATE TABLE course_observations (
  id INTEGER PRIMARY KEY,
  course_id INTEGER NOT NULL REFERENCES courses(id),
  sync_run_id INTEGER NOT NULL REFERENCES sync_runs(id),
  observed_at INTEGER NOT NULL,
  digest TEXT NOT NULL,
  title TEXT NOT NULL,
  code TEXT,
  term TEXT,
  url TEXT NOT NULL
) STRICT;
CREATE INDEX course_observations_course ON course_observations(course_id, id DESC);
CREATE TABLE resources (
  id INTEGER PRIMARY KEY,
  ref TEXT NOT NULL UNIQUE,
  course_id INTEGER NOT NULL REFERENCES courses(id),
  kind TEXT NOT NULL,
  remote_state TEXT NOT NULL DEFAULT 'present'
    CHECK (remote_state IN ('present','not_observed','access_lost')),
  first_seen INTEGER NOT NULL,
  last_seen INTEGER NOT NULL,
  not_observed_since INTEGER
) STRICT;
CREATE INDEX resources_course ON resources(course_id);

CREATE TABLE resource_observations (
  id INTEGER PRIMARY KEY,
  resource_id INTEGER NOT NULL REFERENCES resources(id),
  sync_run_id INTEGER NOT NULL REFERENCES sync_runs(id),
  observed_at INTEGER NOT NULL,
  digest TEXT NOT NULL,
  complete INTEGER NOT NULL,
  title TEXT NOT NULL,
  url TEXT,
  week INTEGER,
  section TEXT,
  text TEXT,
  source_json TEXT NOT NULL
) STRICT;
CREATE INDEX resource_observations_resource ON resource_observations(resource_id, id DESC);

CREATE TABLE representations (
  id INTEGER PRIMARY KEY,
  resource_id INTEGER NOT NULL REFERENCES resources(id),
  url TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('file','link')),
  remote_state TEXT NOT NULL DEFAULT 'present'
    CHECK (remote_state IN ('present','not_observed')),
  first_seen INTEGER NOT NULL,
  last_seen INTEGER NOT NULL,
  not_observed_since INTEGER,
  observed_etag TEXT,
  observed_last_modified TEXT,
  observed_length INTEGER,
  observed_mime TEXT,
  UNIQUE(resource_id, url)
) STRICT;

CREATE TABLE representation_observations (
  id INTEGER PRIMARY KEY,
  representation_id INTEGER NOT NULL REFERENCES representations(id),
  sync_run_id INTEGER NOT NULL REFERENCES sync_runs(id),
  observed_at INTEGER NOT NULL,
  digest TEXT NOT NULL,
  filename TEXT
) STRICT;
CREATE INDEX representation_observations_rep
  ON representation_observations(representation_id, id DESC);

CREATE TABLE blobs (
  sha256 TEXT PRIMARY KEY,
  byte_length INTEGER NOT NULL,
  mime TEXT,
  stored_at INTEGER NOT NULL
) STRICT;
CREATE TABLE content_observations (
  id INTEGER PRIMARY KEY,
  representation_id INTEGER NOT NULL REFERENCES representations(id),
  sync_run_id INTEGER NOT NULL REFERENCES sync_runs(id),
  observed_at INTEGER NOT NULL,
  sha256 TEXT NOT NULL REFERENCES blobs(sha256),
  etag TEXT,
  last_modified TEXT,
  byte_length INTEGER NOT NULL,
  mime TEXT
) STRICT;
CREATE INDEX content_observations_rep ON content_observations(representation_id, id DESC);

CREATE TABLE remote_changes (
  id INTEGER PRIMARY KEY,
  sync_run_id INTEGER NOT NULL REFERENCES sync_runs(id),
  occurred_at INTEGER NOT NULL,
  kind TEXT NOT NULL,
  subject_ref TEXT NOT NULL,
  before_ref TEXT,
  after_ref TEXT,
  details_json TEXT NOT NULL DEFAULT '{}'
) STRICT;
CREATE INDEX remote_changes_time ON remote_changes(id DESC);

CREATE TABLE assertions (
  id INTEGER PRIMARY KEY,
  subject_ref TEXT NOT NULL,
  field TEXT NOT NULL
    CHECK (field IN ('title','filename','summary','note','tag')),
  value TEXT NOT NULL,
  actor TEXT NOT NULL,
  based_on TEXT,
  created_at INTEGER NOT NULL,
  revision INTEGER NOT NULL,
  UNIQUE(subject_ref, field, revision)
) STRICT;

CREATE TABLE relations (
  id INTEGER PRIMARY KEY,
  left_ref TEXT NOT NULL,
  right_ref TEXT NOT NULL,
  kind TEXT NOT NULL
    CHECK (kind IN ('revision_of','duplicate_of','derived_from','related_to')),
  actor TEXT NOT NULL,
  created_at INTEGER NOT NULL
) STRICT;
CREATE INDEX relations_left ON relations(left_ref);
CREATE INDEX relations_right ON relations(right_ref);

CREATE TABLE retractions (
  target_ref TEXT PRIMARY KEY,
  actor TEXT NOT NULL,
  created_at INTEGER NOT NULL
) STRICT;

CREATE VIEW subject_history AS
  SELECT c.ref AS subject_ref, o.observed_at,
         'course_source' AS kind, o.id
    FROM course_observations o
    JOIN courses c ON c.id = o.course_id
  UNION ALL
  SELECT r.ref, o.observed_at, 'resource_source', o.id
    FROM resource_observations o
    JOIN resources r ON r.id = o.resource_id
  UNION ALL
  SELECT 'representation:' || o.representation_id,
         o.observed_at, 'representation_source', o.id
    FROM representation_observations o
  UNION ALL
  SELECT 'representation:' || o.representation_id,
         o.observed_at, 'verified_content', o.id
    FROM content_observations o
  ORDER BY observed_at, kind, id;

CREATE VIRTUAL TABLE search_documents USING fts5(
  subject_ref UNINDEXED,
  kind UNINDEXED,
  course UNINDEXED,
  title,
  body,
  tags,
  tokenize = 'unicode61'
);
"#;
