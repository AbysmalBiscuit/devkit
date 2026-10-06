-- repo-rules-agent's Postgres query schema, copied verbatim from its
-- src/rules_agent/storage/sql/postgres/001_schema.sql. The tests load it alone;
-- 002_supabase.sql adds grants, policies and RPCs devkit does not use.

CREATE SCHEMA repo_rules;

CREATE SCHEMA repo_rules_archive;

CREATE SCHEMA repo_rules_private;

CREATE TABLE repo_rules.repositories (
	repo_id UUID NOT NULL,
	locator TEXT NOT NULL,
	local_path TEXT,
	source_sha TEXT NOT NULL,
	active_generation UUID,
	revision BIGINT DEFAULT '0' NOT NULL,
	conflict_count INTEGER DEFAULT '0' NOT NULL,
	CONSTRAINT pk_repositories PRIMARY KEY (repo_id),
	CONSTRAINT ck_repositories_revision CHECK (revision >= 0),
	CONSTRAINT ck_repositories_conflicts CHECK (conflict_count >= 0),
	CONSTRAINT uq_repositories_locator UNIQUE (locator)
);

CREATE TABLE repo_rules.storage_version (
	singleton SERIAL NOT NULL,
	version INTEGER NOT NULL,
	CONSTRAINT pk_storage_version PRIMARY KEY (singleton),
	CONSTRAINT ck_storage_version_singleton CHECK (singleton = 1)
);

CREATE TABLE repo_rules_archive.extractions (
	repo_id UUID NOT NULL,
	generation UUID NOT NULL,
	repo_path TEXT NOT NULL,
	conflicts JSONB NOT NULL,
	extra JSONB NOT NULL,
	CONSTRAINT pk_extractions PRIMARY KEY (repo_id)
);

CREATE TABLE repo_rules_archive.storage_version (
	singleton SERIAL NOT NULL,
	version INTEGER NOT NULL,
	CONSTRAINT pk_storage_version PRIMARY KEY (singleton),
	CONSTRAINT ck_storage_version_singleton CHECK (singleton = 1)
);

CREATE TABLE repo_rules.sources (
	repo_id UUID NOT NULL,
	source_key UUID NOT NULL,
	path TEXT NOT NULL,
	tier INTEGER NOT NULL,
	discovered BOOLEAN NOT NULL,
	extra JSONB NOT NULL,
	CONSTRAINT pk_sources PRIMARY KEY (repo_id, source_key),
	CONSTRAINT fk_sources_repo_id_repositories FOREIGN KEY(repo_id) REFERENCES repo_rules.repositories (repo_id),
	CONSTRAINT uq_sources_repo_id UNIQUE (repo_id, path)
);

CREATE TABLE repo_rules_archive.extraction_files (
	repo_id UUID NOT NULL,
	position INTEGER NOT NULL,
	payload JSONB NOT NULL,
	CONSTRAINT pk_extraction_files PRIMARY KEY (repo_id, position),
	CONSTRAINT fk_extraction_files_repo_id_extractions FOREIGN KEY(repo_id) REFERENCES repo_rules_archive.extractions (repo_id) ON DELETE CASCADE,
	CONSTRAINT ck_extraction_files_position CHECK (position >= 0)
);

CREATE TABLE repo_rules_private.repository_members (
	repo_id UUID NOT NULL,
	principal_id UUID NOT NULL,
	role TEXT NOT NULL,
	CONSTRAINT pk_repository_members PRIMARY KEY (repo_id, principal_id),
	CONSTRAINT fk_repository_members_repo_id_repositories FOREIGN KEY(repo_id) REFERENCES repo_rules.repositories (repo_id) ON DELETE CASCADE,
	CONSTRAINT ck_repository_members_role CHECK (role IN ('reader', 'editor', 'owner'))
);

CREATE TABLE repo_rules.rules (
	repo_id UUID NOT NULL,
	rule_key UUID NOT NULL,
	external_id TEXT NOT NULL,
	source_key UUID NOT NULL,
	title TEXT NOT NULL,
	description TEXT NOT NULL,
	category TEXT NOT NULL,
	scope TEXT NOT NULL,
	severity TEXT NOT NULL,
	directory TEXT,
	position BIGINT NOT NULL,
	pinned BOOLEAN DEFAULT 'false' NOT NULL,
	removed BOOLEAN DEFAULT 'false' NOT NULL,
	extraction_fingerprint TEXT,
	extra JSONB NOT NULL,
	CONSTRAINT pk_rules PRIMARY KEY (repo_id, rule_key),
	CONSTRAINT fk_rules_repo_id_repositories FOREIGN KEY(repo_id) REFERENCES repo_rules.repositories (repo_id),
	CONSTRAINT fk_rules_repo_id_sources FOREIGN KEY(repo_id, source_key) REFERENCES repo_rules.sources (repo_id, source_key),
	CONSTRAINT ck_rules_scope CHECK (scope IN ('repo', 'directory', 'file-pattern')),
	CONSTRAINT ck_rules_severity CHECK (severity IN ('must', 'should', 'can')),
	CONSTRAINT ck_rules_position CHECK (position >= 0),
	CONSTRAINT ck_rules_tombstone CHECK (NOT removed OR pinned)
);

CREATE TABLE repo_rules.rule_languages (
	repo_id UUID NOT NULL,
	rule_key UUID NOT NULL,
	position INTEGER NOT NULL,
	language TEXT NOT NULL,
	CONSTRAINT pk_rule_languages PRIMARY KEY (repo_id, rule_key, position),
	CONSTRAINT ck_rule_languages_position CHECK (position >= 0),
	CONSTRAINT fk_rule_languages_repo_id_rules FOREIGN KEY(repo_id, rule_key) REFERENCES repo_rules.rules (repo_id, rule_key) ON DELETE CASCADE
);

CREATE TABLE repo_rules.rule_tasks (
	repo_id UUID NOT NULL,
	rule_key UUID NOT NULL,
	position INTEGER NOT NULL,
	task TEXT NOT NULL,
	CONSTRAINT pk_rule_tasks PRIMARY KEY (repo_id, rule_key, position),
	CONSTRAINT ck_rule_tasks_position CHECK (position >= 0),
	CONSTRAINT ck_rule_tasks_task CHECK (task IN ('code-review', 'code-generation', 'code-questions')),
	CONSTRAINT fk_rule_tasks_repo_id_rules FOREIGN KEY(repo_id, rule_key) REFERENCES repo_rules.rules (repo_id, rule_key) ON DELETE CASCADE
);

CREATE INDEX principal_lookup ON repo_rules_private.repository_members (principal_id, repo_id);

CREATE INDEX external_id_lookup ON repo_rules.rules (repo_id, external_id);

CREATE INDEX pins_lookup ON repo_rules.rules (repo_id, pinned, extraction_fingerprint);

CREATE INDEX scope_lookup ON repo_rules.rules (repo_id, scope, removed);

CREATE INDEX severity_lookup ON repo_rules.rules (repo_id, severity, removed);

CREATE INDEX visible_order ON repo_rules.rules (repo_id, removed, position, rule_key);

CREATE INDEX languages_lookup ON repo_rules.rule_languages (repo_id, language, rule_key);

CREATE INDEX tasks_lookup ON repo_rules.rule_tasks (repo_id, task, rule_key);

INSERT INTO repo_rules.storage_version VALUES (1, 1);

INSERT INTO repo_rules_archive.storage_version VALUES (1, 1);
