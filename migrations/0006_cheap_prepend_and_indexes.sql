-- Prepend by taking min(position) - 1 instead of shifting every row of the user on each insert.
-- Positions may go negative; the "custom" sort key offsets them (see bookmarks/service.rs).
CREATE OR REPLACE FUNCTION bookmarks_prepend_position() RETURNS trigger AS $$
BEGIN
    IF NEW.client_request_id IS NOT NULL AND EXISTS (
        SELECT 1 FROM bookmarks WHERE user_id = NEW.user_id AND client_request_id = NEW.client_request_id
    ) THEN
        RETURN NEW;
    END IF;
    PERFORM id FROM users WHERE id=NEW.user_id FOR UPDATE;
    SELECT coalesce(min(position), 1) - 1 INTO NEW.position FROM bookmarks WHERE user_id = NEW.user_id;
    RETURN NEW;
END
$$ LANGUAGE plpgsql;

CREATE INDEX IF NOT EXISTS idx_folder_members_user ON folder_members(user_id);
CREATE INDEX IF NOT EXISTS idx_bookmarks_folder_active ON bookmarks(folder_id) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_folders_parent ON folders(parent_id);
CREATE INDEX IF NOT EXISTS idx_media_assets_bookmark ON media_assets(bookmark_id);
