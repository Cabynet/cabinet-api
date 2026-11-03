-- Link existing uploads to the bookmark that points at them (raw_url ends with the object key).
-- The owner's oldest matching bookmark wins; later copies by other users are deliberately not linked.
UPDATE media_assets m SET bookmark_id = b.id
FROM (
    SELECT DISTINCT ON (m2.id) m2.id AS asset_id, b2.id
    FROM media_assets m2
    JOIN bookmarks b2 ON b2.user_id = m2.user_id
     AND b2.raw_url IS NOT NULL
     AND right(b2.raw_url, length(m2.object_key)) = m2.object_key
    WHERE m2.bookmark_id IS NULL AND m2.status = 'READY'
    ORDER BY m2.id, b2.created_at, b2.id
) b
WHERE m.id = b.asset_id;
