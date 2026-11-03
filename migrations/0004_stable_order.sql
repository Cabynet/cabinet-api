-- Preserve the existing relative custom order while removing ambiguous position ties.
WITH ranked AS (
    SELECT id, (row_number() OVER (PARTITION BY user_id ORDER BY position, created_at DESC, id DESC) - 1)::int AS position
    FROM bookmarks
)
UPDATE bookmarks b SET position = r.position FROM ranked r WHERE b.id = r.id AND b.position IS DISTINCT FROM r.position;
CREATE INDEX IF NOT EXISTS idx_bookmarks_position ON bookmarks(user_id, position);
CREATE OR REPLACE FUNCTION bookmarks_append_position() RETURNS trigger AS $$
BEGIN
    PERFORM id FROM users WHERE id=NEW.user_id FOR UPDATE;
    SELECT coalesce(max(position), -1) + 1 INTO NEW.position FROM bookmarks WHERE user_id=NEW.user_id;
    RETURN NEW;
END
$$ LANGUAGE plpgsql;
CREATE TRIGGER trg_bookmarks_append_position BEFORE INSERT ON bookmarks FOR EACH ROW EXECUTE FUNCTION bookmarks_append_position();
