-- One link per folder, like Google Drive's "Anyone with the link": the role says what
-- the link grants. VIEWER opens read-only without sign-in; CONTRIBUTOR and EDITOR
-- also let a signed-in visitor join the folder with that role.
ALTER TABLE folders ADD COLUMN IF NOT EXISTS share_role VARCHAR(16) NOT NULL DEFAULT 'VIEWER'
    CHECK (share_role IN ('VIEWER', 'CONTRIBUTOR', 'EDITOR'));
