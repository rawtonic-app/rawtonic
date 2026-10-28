DROP TRIGGER versions_ai_facts_ai;
DROP TRIGGER versions_ai_facts_au;
DROP TRIGGER recipes_ai_facts_au;
DROP INDEX versions_ai_facts;
ALTER TABLE versions DROP COLUMN ai_facts;
ALTER TABLE assets DROP COLUMN source_type;
PRAGMA user_version=16;
