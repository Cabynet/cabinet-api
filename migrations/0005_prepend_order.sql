-- Prepend newly created bookmarks at position 0 so they always appear first in Custom Order.
CREATE OR REPLACE FUNCTION bookmarks_prepend_position() RETURNS trigger AS $$
BEGIN
    IF NEW.client_request_id IS NOT NULL AND EXISTS (
        SELECT 1 FROM bookmarks WHERE user_id = NEW.user_id AND client_request_id = NEW.client_request_id
    ) THEN
        RETURN NEW;
    END IF;
    PERFORM id FROM users WHERE id=NEW.user_id FOR UPDATE;
    UPDATE bookmarks SET position = position + 1 WHERE user_id = NEW.user_id;
    NEW.position = 0;
    RETURN NEW;
END
$$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS trg_bookmarks_append_position ON bookmarks;
DROP TRIGGER IF EXISTS trg_bookmarks_prepend_position ON bookmarks;
CREATE TRIGGER trg_bookmarks_prepend_position BEFORE INSERT ON bookmarks FOR EACH ROW EXECUTE FUNCTION bookmarks_prepend_position();

-- Re-rank existing bookmarks so the newest created items have position 0, 1, 2...
WITH ranked AS (
    SELECT id, (row_number() OVER (PARTITION BY user_id ORDER BY created_at DESC, id DESC) - 1)::int AS new_pos
    FROM bookmarks
)
UPDATE bookmarks b SET position = r.new_pos FROM ranked r WHERE b.id = r.id;
