DROP TRIGGER stack_members_copy_gone;
DROP TRIGGER stack_members_dissolve;
DROP TABLE stack_members;
DROP TABLE stacks;
ALTER TABLE assets DROP COLUMN sequence;
ALTER TABLE assets DROP COLUMN exposure_bias;
ALTER TABLE assets DROP COLUMN exposure_mode;
PRAGMA user_version=21;
