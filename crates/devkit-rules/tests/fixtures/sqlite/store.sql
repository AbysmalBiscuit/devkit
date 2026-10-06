BEGIN TRANSACTION;
CREATE TABLE repositories (
	repo_id CHAR(32) NOT NULL, 
	locator TEXT NOT NULL, 
	local_path TEXT, 
	source_sha TEXT NOT NULL, 
	active_generation CHAR(32), 
	revision BIGINT DEFAULT '0' NOT NULL, 
	conflict_count INTEGER DEFAULT '0' NOT NULL, 
	CONSTRAINT pk_repositories PRIMARY KEY (repo_id), 
	CONSTRAINT ck_repositories_revision CHECK (revision >= 0), 
	CONSTRAINT ck_repositories_conflicts CHECK (conflict_count >= 0), 
	CONSTRAINT uq_repositories_locator UNIQUE (locator)
);
INSERT INTO "repositories" VALUES('dce0239e128d477187944574ea36bd72','local:/repo','/repo','fixture','f0e86c82eb214ca29570ee50bbc75463',1,0);
CREATE TABLE repository_members (
	repo_id CHAR(32) NOT NULL, 
	principal_id CHAR(32) NOT NULL, 
	role TEXT NOT NULL, 
	CONSTRAINT pk_repository_members PRIMARY KEY (repo_id, principal_id), 
	CONSTRAINT ck_repository_members_role CHECK (role IN ('reader', 'editor', 'owner'))
);
CREATE TABLE rule_languages (
	repo_id CHAR(32) NOT NULL, 
	rule_key CHAR(32) NOT NULL, 
	position INTEGER NOT NULL, 
	language TEXT NOT NULL, 
	CONSTRAINT pk_rule_languages PRIMARY KEY (repo_id, rule_key, position), 
	CONSTRAINT ck_rule_languages_position CHECK (position >= 0), 
	CONSTRAINT fk_rule_languages_repo_id_rules FOREIGN KEY(repo_id, rule_key) REFERENCES rules (repo_id, rule_key) ON DELETE CASCADE
)
 WITHOUT ROWID

;
INSERT INTO "rule_languages" VALUES('dce0239e128d477187944574ea36bd72','17964b6f3c214c878ee22e2057e3aaee',0,'all');
INSERT INTO "rule_languages" VALUES('dce0239e128d477187944574ea36bd72','3b811f6d846844a09a7f529270d901c2',0,'all');
INSERT INTO "rule_languages" VALUES('dce0239e128d477187944574ea36bd72','4286f97f4a35455fb25bf104307e2cd1',0,'all');
INSERT INTO "rule_languages" VALUES('dce0239e128d477187944574ea36bd72','62424d445ade4e6394e17004a06acb53',0,'all');
INSERT INTO "rule_languages" VALUES('dce0239e128d477187944574ea36bd72','7bd7282baee14eeaaf5a2640f6ac9379',0,'all');
INSERT INTO "rule_languages" VALUES('dce0239e128d477187944574ea36bd72','1fa0e482d74d4ac593c582195e5e721b',0,'rust');
INSERT INTO "rule_languages" VALUES('dce0239e128d477187944574ea36bd72','1fa0e482d74d4ac593c582195e5e721b',1,'toml');
INSERT INTO "rule_languages" VALUES('dce0239e128d477187944574ea36bd72','775a2cd87d1647a4a7c96ffffd565dca',0,'typescript');
CREATE TABLE rule_tasks (
	repo_id CHAR(32) NOT NULL, 
	rule_key CHAR(32) NOT NULL, 
	position INTEGER NOT NULL, 
	task TEXT NOT NULL, 
	CONSTRAINT pk_rule_tasks PRIMARY KEY (repo_id, rule_key, position), 
	CONSTRAINT ck_rule_tasks_position CHECK (position >= 0), 
	CONSTRAINT ck_rule_tasks_task CHECK (task IN ('code-review', 'code-generation', 'code-questions')), 
	CONSTRAINT fk_rule_tasks_repo_id_rules FOREIGN KEY(repo_id, rule_key) REFERENCES rules (repo_id, rule_key) ON DELETE CASCADE
)
 WITHOUT ROWID

;
INSERT INTO "rule_tasks" VALUES('dce0239e128d477187944574ea36bd72','1fa0e482d74d4ac593c582195e5e721b',1,'code-generation');
INSERT INTO "rule_tasks" VALUES('dce0239e128d477187944574ea36bd72','3b811f6d846844a09a7f529270d901c2',0,'code-generation');
INSERT INTO "rule_tasks" VALUES('dce0239e128d477187944574ea36bd72','4286f97f4a35455fb25bf104307e2cd1',0,'code-generation');
INSERT INTO "rule_tasks" VALUES('dce0239e128d477187944574ea36bd72','62424d445ade4e6394e17004a06acb53',0,'code-generation');
INSERT INTO "rule_tasks" VALUES('dce0239e128d477187944574ea36bd72','7bd7282baee14eeaaf5a2640f6ac9379',0,'code-generation');
INSERT INTO "rule_tasks" VALUES('dce0239e128d477187944574ea36bd72','17964b6f3c214c878ee22e2057e3aaee',0,'code-review');
INSERT INTO "rule_tasks" VALUES('dce0239e128d477187944574ea36bd72','1fa0e482d74d4ac593c582195e5e721b',0,'code-review');
CREATE TABLE rules (
	repo_id CHAR(32) NOT NULL, 
	rule_key CHAR(32) NOT NULL, 
	external_id TEXT NOT NULL, 
	source_key CHAR(32) NOT NULL, 
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
	extra JSON NOT NULL, 
	CONSTRAINT pk_rules PRIMARY KEY (repo_id, rule_key), 
	CONSTRAINT fk_rules_repo_id_repositories FOREIGN KEY(repo_id) REFERENCES repositories (repo_id), 
	CONSTRAINT fk_rules_repo_id_sources FOREIGN KEY(repo_id, source_key) REFERENCES sources (repo_id, source_key), 
	CONSTRAINT ck_rules_scope CHECK (scope IN ('repo', 'directory', 'file-pattern')), 
	CONSTRAINT ck_rules_severity CHECK (severity IN ('must', 'should', 'can')), 
	CONSTRAINT ck_rules_position CHECK (position >= 0), 
	CONSTRAINT ck_rules_tombstone CHECK (NOT removed OR pinned)
);
INSERT INTO "rules" VALUES('dce0239e128d477187944574ea36bd72','4286f97f4a35455fb25bf104307e2cd1','r-root-must','b2c9fe8fdeb04c8eabc23bd4de4cbcd8','Root must','Applies everywhere.','security','repo','must','',0,0,0,'396c344b1cc803319bd97cb8fb5b4abf7057ede32a37f19f3803e289012e006f','{"topics": []}');
INSERT INTO "rules" VALUES('dce0239e128d477187944574ea36bd72','1fa0e482d74d4ac593c582195e5e721b','r-foo-should','26520dc2bd1a495f92bab88a544f7b04','Foo should','Rust only, under crates/foo.','code_style','directory','should','crates/foo',1,0,0,'26eb7cb3973c36f22d8f73ae79be0bd1507dcbc7fe3381649f88337245ef4a4a','{"topics": ["errors"], "future_field": 7}');
INSERT INTO "rules" VALUES('dce0239e128d477187944574ea36bd72','62424d445ade4e6394e17004a06acb53','r-gone','b2c9fe8fdeb04c8eabc23bd4de4cbcd8','Gone','Removed by a person.','best_practice','repo','must','',2,1,1,NULL,'{"topics": []}');
INSERT INTO "rules" VALUES('dce0239e128d477187944574ea36bd72','3b811f6d846844a09a7f529270d901c2','r-foo-can','26520dc2bd1a495f92bab88a544f7b04','Foo can','Optional, under crates/foo.','readability','directory','can','crates/foo',3,0,0,'213c2d7d02f2bead9b8fc1755da4ff7e8b9db111d025eab4a6590f13c49c676f','{"topics": ["readability"]}');
INSERT INTO "rules" VALUES('dce0239e128d477187944574ea36bd72','775a2cd87d1647a4a7c96ffffd565dca','r-untagged','b2c9fe8fdeb04c8eabc23bd4de4cbcd8','Untagged','No tasks listed.','best_practice','repo','should','',4,0,0,'2c91b09d25280049389e2eeccb4c266e91118156398f20354c3984eaa42d0d2c','{"topics": []}');
INSERT INTO "rules" VALUES('dce0239e128d477187944574ea36bd72','17964b6f3c214c878ee22e2057e3aaee','r-review-only','b2c9fe8fdeb04c8eabc23bd4de4cbcd8','Review only','Code review task only.','best_practice','repo','must','',5,0,0,'8c87b1d8d0c3776a42b063a19219858891c4681b9eaa637686800f6e80aca3b5','{"topics": []}');
INSERT INTO "rules" VALUES('dce0239e128d477187944574ea36bd72','7bd7282baee14eeaaf5a2640f6ac9379','r-pinned-manual','b588e8e2c8984d15b7532c4a3a50e674','Pinned manual','Added by a person.','best_practice','repo','should','',6,1,0,NULL,'{"topics": []}');
CREATE TABLE sources (
	repo_id CHAR(32) NOT NULL, 
	source_key CHAR(32) NOT NULL, 
	path TEXT NOT NULL, 
	tier INTEGER NOT NULL, 
	discovered BOOLEAN NOT NULL, 
	extra JSON NOT NULL, 
	CONSTRAINT pk_sources PRIMARY KEY (repo_id, source_key), 
	CONSTRAINT fk_sources_repo_id_repositories FOREIGN KEY(repo_id) REFERENCES repositories (repo_id), 
	CONSTRAINT uq_sources_repo_id UNIQUE (repo_id, path)
);
INSERT INTO "sources" VALUES('dce0239e128d477187944574ea36bd72','b2c9fe8fdeb04c8eabc23bd4de4cbcd8','AGENTS.md',1,1,'{"file_position": 0}');
INSERT INTO "sources" VALUES('dce0239e128d477187944574ea36bd72','26520dc2bd1a495f92bab88a544f7b04','crates/foo/AGENTS.md',2,1,'{"file_position": 1, "errors": ["chunk 3: timed out"]}');
INSERT INTO "sources" VALUES('dce0239e128d477187944574ea36bd72','b588e8e2c8984d15b7532c4a3a50e674','',0,0,'{}');
CREATE TABLE storage_version (
	singleton INTEGER NOT NULL, 
	version INTEGER NOT NULL, 
	CONSTRAINT pk_storage_version PRIMARY KEY (singleton), 
	CONSTRAINT ck_storage_version_singleton CHECK (singleton = 1)
);
INSERT INTO "storage_version" VALUES(1,1);
CREATE INDEX principal_lookup ON repository_members (principal_id, repo_id);
CREATE INDEX severity_lookup ON rules (repo_id, severity, removed);
CREATE INDEX pins_lookup ON rules (repo_id, pinned, extraction_fingerprint);
CREATE INDEX external_id_lookup ON rules (repo_id, external_id);
CREATE INDEX scope_lookup ON rules (repo_id, scope, removed);
CREATE INDEX visible_order ON rules (repo_id, removed, position, rule_key);
CREATE INDEX languages_lookup ON rule_languages (repo_id, language, rule_key);
CREATE INDEX tasks_lookup ON rule_tasks (repo_id, task, rule_key);
COMMIT;
