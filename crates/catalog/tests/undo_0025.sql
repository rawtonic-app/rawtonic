ALTER TABLE face_eyes DROP COLUMN focus_override;
ALTER TABLE face_eyes DROP COLUMN focus_sigma;
ALTER TABLE face_eyes DROP COLUMN focus;
PRAGMA user_version=24;
