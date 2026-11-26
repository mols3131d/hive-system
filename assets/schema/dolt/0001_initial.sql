-- Hive Dolt schema version 1. Apply only during explicit bootstrap.
-- Workspace identity, the migration ledger row, and counters are populated separately.

CREATE TABLE areas (
  id CHAR(36) PRIMARY KEY,
  parent_id CHAR(36),
  slug VARCHAR(100) NOT NULL,
  name VARCHAR(255) NOT NULL,
  description TEXT,
  archived_at DATETIME(6),
  revision CHAR(36) NOT NULL,
  UNIQUE (parent_id, slug),
  FOREIGN KEY (parent_id) REFERENCES areas(id),
  CHECK (parent_id IS NULL OR parent_id <> id)
);

CREATE TABLE projects (
  id CHAR(36) PRIMARY KEY,
  name VARCHAR(255) NOT NULL,
  description TEXT,
  status VARCHAR(16) NOT NULL,
  revision CHAR(36) NOT NULL,
  CHECK (status IN ('active', 'completed', 'archived'))
);

CREATE TABLE issues (
  id CHAR(36) PRIMARY KEY,
  number BIGINT UNSIGNED NOT NULL UNIQUE,
  area_id CHAR(36) NOT NULL,
  parent_id CHAR(36),
  title VARCHAR(255) NOT NULL,
  body LONGTEXT,
  acceptance_criteria TEXT,
  type VARCHAR(16) NOT NULL,
  status VARCHAR(16) NOT NULL,
  priority TINYINT,
  hold_reason TEXT,
  revision CHAR(36) NOT NULL,
  created_at DATETIME(6) NOT NULL,
  updated_at DATETIME(6) NOT NULL,
  closed_at DATETIME(6),
  closed_by_ref VARCHAR(255),
  resolution_note TEXT,
  FOREIGN KEY (area_id) REFERENCES areas(id),
  FOREIGN KEY (parent_id) REFERENCES issues(id),
  CHECK (parent_id IS NULL OR parent_id <> id),
  CHECK (number > 0),
  CHECK (type IN ('epic','feature','task','bug')),
  CHECK (type <> 'epic' OR status <> 'in-progress'),
  CHECK (status IN ('backlog','todo','in-progress','review','done','cancelled')),
  CHECK (priority IS NULL OR priority BETWEEN 0 AND 3),
  CHECK (
    (status IN ('done','cancelled') AND closed_at IS NOT NULL
      AND closed_by_ref IS NOT NULL AND resolution_note IS NOT NULL)
    OR
    (status NOT IN ('done','cancelled') AND closed_at IS NULL
      AND closed_by_ref IS NULL AND resolution_note IS NULL)
  ),
  INDEX issues_area_status (area_id, status, priority)
);

CREATE TABLE project_issues (
  project_id CHAR(36) NOT NULL,
  issue_id CHAR(36) NOT NULL,
  PRIMARY KEY (project_id, issue_id),
  FOREIGN KEY (project_id) REFERENCES projects(id),
  FOREIGN KEY (issue_id) REFERENCES issues(id),
  INDEX project_issues_reverse (issue_id, project_id)
);

CREATE TABLE issue_dependencies (
  issue_id CHAR(36) NOT NULL,
  prerequisite_id CHAR(36) NOT NULL,
  PRIMARY KEY (issue_id, prerequisite_id),
  FOREIGN KEY (issue_id) REFERENCES issues(id),
  FOREIGN KEY (prerequisite_id) REFERENCES issues(id),
  CHECK (issue_id <> prerequisite_id),
  INDEX dependency_reverse (prerequisite_id, issue_id)
);

CREATE TABLE resources (
  id CHAR(36) PRIMARY KEY,
  kind VARCHAR(32) NOT NULL,
  name VARCHAR(255) NOT NULL,
  uri TEXT,
  revision CHAR(36) NOT NULL
);

CREATE TABLE area_resources (
  area_id CHAR(36) NOT NULL,
  resource_id CHAR(36) NOT NULL,
  PRIMARY KEY (area_id, resource_id),
  FOREIGN KEY (area_id) REFERENCES areas(id),
  FOREIGN KEY (resource_id) REFERENCES resources(id),
  INDEX area_resources_reverse (resource_id, area_id)
);

CREATE TABLE issue_resources (
  issue_id CHAR(36) NOT NULL,
  resource_id CHAR(36) NOT NULL,
  PRIMARY KEY (issue_id, resource_id),
  FOREIGN KEY (issue_id) REFERENCES issues(id),
  FOREIGN KEY (resource_id) REFERENCES resources(id),
  INDEX issue_resources_reverse (resource_id, issue_id)
);

CREATE TABLE issue_claims (
  issue_id CHAR(36) PRIMARY KEY,
  claim_id CHAR(36) NOT NULL UNIQUE,
  holder_ref VARCHAR(255) NOT NULL,
  acquired_at DATETIME(6) NOT NULL,
  FOREIGN KEY (issue_id) REFERENCES issues(id)
);

CREATE TABLE workspace_counters (
  name VARCHAR(32) PRIMARY KEY,
  next_value BIGINT UNSIGNED NOT NULL
);

CREATE TABLE operation_receipts (
  request_id CHAR(36) PRIMARY KEY,
  kind VARCHAR(64) NOT NULL,
  fingerprint CHAR(64) NOT NULL,
  actor_ref VARCHAR(255) NOT NULL,
  authorization_ref TEXT NOT NULL,
  workflow_evidence_json LONGTEXT NOT NULL,
  base_commit VARCHAR(128) NOT NULL,
  result_json LONGTEXT NOT NULL,
  created_at DATETIME(6) NOT NULL
);

CREATE TABLE workspace_meta (
  singleton_id TINYINT PRIMARY KEY,
  workspace_id CHAR(36) NOT NULL UNIQUE,
  CHECK (singleton_id = 1)
);

CREATE TABLE schema_migrations (
  version INT UNSIGNED PRIMARY KEY,
  migration_digest CHAR(64) NOT NULL,
  applied_at DATETIME(6) NOT NULL
);

CREATE TABLE labels (
  id CHAR(36) PRIMARY KEY,
  name VARCHAR(100) NOT NULL UNIQUE,
  description TEXT,
  revision CHAR(36) NOT NULL
);

CREATE TABLE issue_labels (
  issue_id CHAR(36) NOT NULL,
  label_id CHAR(36) NOT NULL,
  PRIMARY KEY (issue_id, label_id),
  FOREIGN KEY (issue_id) REFERENCES issues(id),
  FOREIGN KEY (label_id) REFERENCES labels(id),
  INDEX issue_labels_reverse (label_id, issue_id)
);

CREATE TABLE issue_updates (
  id CHAR(36) PRIMARY KEY,
  issue_id CHAR(36) NOT NULL,
  kind VARCHAR(16) NOT NULL,
  body LONGTEXT NOT NULL,
  author_ref VARCHAR(255) NOT NULL,
  created_at DATETIME(6) NOT NULL,
  transition_json LONGTEXT,
  FOREIGN KEY (issue_id) REFERENCES issues(id),
  CHECK (kind IN ('progress','review','handoff','evidence','comment','transition')),
  CHECK (
    (kind = 'transition' AND transition_json IS NOT NULL)
    OR (kind <> 'transition' AND transition_json IS NULL)
  ),
  INDEX issue_updates_order (issue_id, created_at, id)
);

CREATE TABLE bots (
  id CHAR(36) PRIMARY KEY,
  slug VARCHAR(100) NOT NULL UNIQUE,
  display_name VARCHAR(255) NOT NULL,
  revision CHAR(36) NOT NULL,
  retired_at DATETIME(6)
);

CREATE TABLE bot_memories (
  id CHAR(36) PRIMARY KEY,
  bot_id CHAR(36) NOT NULL,
  area_id CHAR(36),
  kind VARCHAR(16) NOT NULL,
  recall_tier VARCHAR(16),
  topic_key VARCHAR(100) NOT NULL,
  title VARCHAR(255) NOT NULL,
  body LONGTEXT NOT NULL,
  source_ref TEXT NOT NULL,
  author_ref VARCHAR(255) NOT NULL,
  authorization_mode VARCHAR(16) NOT NULL,
  authorization_ref TEXT NOT NULL,
  reviewed_by_ref VARCHAR(255),
  observed_at DATETIME(6),
  reviewed_at DATETIME(6),
  revision CHAR(36) NOT NULL,
  created_at DATETIME(6) NOT NULL,
  updated_at DATETIME(6) NOT NULL,
  retired_at DATETIME(6),
  retirement_reason TEXT,
  FOREIGN KEY (bot_id) REFERENCES bots(id),
  FOREIGN KEY (area_id) REFERENCES areas(id),
  CHECK (
    (kind = 'latest' AND recall_tier IS NULL)
    OR (
      kind = 'durable'
      AND recall_tier IS NOT NULL
      AND recall_tier IN ('core','reference')
    )
  ),
  CHECK (
    (authorization_mode = 'reviewed'
      AND reviewed_by_ref IS NOT NULL AND reviewed_at IS NOT NULL)
    OR (authorization_mode = 'delegated'
      AND reviewed_by_ref IS NULL AND reviewed_at IS NULL)
  ),
  CHECK (
    (retired_at IS NULL AND retirement_reason IS NULL)
    OR (retired_at IS NOT NULL AND retirement_reason IS NOT NULL)
  ),
  INDEX bot_memories_recall (bot_id, area_id, retired_at, kind, recall_tier, id),
  INDEX bot_memories_topic (bot_id, area_id, topic_key)
);

CREATE TABLE directives (
  id CHAR(36) PRIMARY KEY,
  scope_kind VARCHAR(16) NOT NULL,
  area_id CHAR(36),
  project_id CHAR(36),
  include_descendants BOOLEAN NOT NULL DEFAULT FALSE,
  title VARCHAR(255) NOT NULL,
  body LONGTEXT NOT NULL,
  status VARCHAR(16) NOT NULL,
  issuer_ref VARCHAR(255) NOT NULL,
  source_ref TEXT NOT NULL,
  approval_ref TEXT,
  valid_from DATETIME(6),
  expires_at DATETIME(6),
  supersedes_id CHAR(36),
  revision CHAR(36) NOT NULL,
  created_at DATETIME(6) NOT NULL,
  updated_at DATETIME(6) NOT NULL,
  FOREIGN KEY (area_id) REFERENCES areas(id),
  FOREIGN KEY (project_id) REFERENCES projects(id),
  FOREIGN KEY (supersedes_id) REFERENCES directives(id),
  CHECK (
    (scope_kind = 'workspace' AND area_id IS NULL AND project_id IS NULL)
    OR (scope_kind = 'area' AND area_id IS NOT NULL AND project_id IS NULL)
    OR (scope_kind = 'project' AND area_id IS NULL AND project_id IS NOT NULL)
  ),
  CHECK (scope_kind = 'area' OR include_descendants = FALSE),
  CHECK (status IN ('proposed','active','superseded','withdrawn')),
  CHECK (status <> 'active' OR approval_ref IS NOT NULL),
  CHECK (valid_from IS NULL OR expires_at IS NULL OR expires_at > valid_from),
  CHECK (supersedes_id IS NULL OR supersedes_id <> id)
);

CREATE TABLE artifacts (
  id CHAR(36) PRIMARY KEY,
  kind VARCHAR(32) NOT NULL,
  title VARCHAR(255) NOT NULL,
  body LONGTEXT NOT NULL,
  author_ref VARCHAR(255) NOT NULL,
  source_ref TEXT,
  revision CHAR(36) NOT NULL,
  created_at DATETIME(6) NOT NULL,
  substantive_updated_at DATETIME(6) NOT NULL,
  archived_at DATETIME(6),
  archive_reason TEXT,
  CHECK (
    (archived_at IS NULL AND archive_reason IS NULL)
    OR (archived_at IS NOT NULL AND archive_reason IS NOT NULL)
  )
);

CREATE TABLE artifact_areas (
  artifact_id CHAR(36) NOT NULL,
  area_id CHAR(36) NOT NULL,
  PRIMARY KEY (artifact_id, area_id),
  FOREIGN KEY (artifact_id) REFERENCES artifacts(id),
  FOREIGN KEY (area_id) REFERENCES areas(id),
  INDEX artifact_areas_reverse (area_id, artifact_id)
);

CREATE TABLE artifact_projects (
  artifact_id CHAR(36) NOT NULL,
  project_id CHAR(36) NOT NULL,
  PRIMARY KEY (artifact_id, project_id),
  FOREIGN KEY (artifact_id) REFERENCES artifacts(id),
  FOREIGN KEY (project_id) REFERENCES projects(id),
  INDEX artifact_projects_reverse (project_id, artifact_id)
);

CREATE TABLE artifact_issues (
  artifact_id CHAR(36) NOT NULL,
  issue_id CHAR(36) NOT NULL,
  PRIMARY KEY (artifact_id, issue_id),
  FOREIGN KEY (artifact_id) REFERENCES artifacts(id),
  FOREIGN KEY (issue_id) REFERENCES issues(id),
  INDEX artifact_issues_reverse (issue_id, artifact_id)
);

CREATE TABLE artifact_resources (
  artifact_id CHAR(36) NOT NULL,
  resource_id CHAR(36) NOT NULL,
  PRIMARY KEY (artifact_id, resource_id),
  FOREIGN KEY (artifact_id) REFERENCES artifacts(id),
  FOREIGN KEY (resource_id) REFERENCES resources(id),
  INDEX artifact_resources_reverse (resource_id, artifact_id)
);
