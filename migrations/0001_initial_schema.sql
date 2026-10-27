-- 0001_initial_schema.sql
-- Cabinet PostgreSQL 17 Initial Migration

CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- 1. Users
CREATE TABLE IF NOT EXISTS users (
    id UUID PRIMARY KEY,
    email TEXT NOT NULL UNIQUE,
    email_verified BOOLEAN NOT NULL DEFAULT FALSE,
    password_hash TEXT,
    name TEXT NOT NULL,
    avatar_url TEXT,
    storage_used_bytes BIGINT NOT NULL DEFAULT 0,
    storage_quota_bytes BIGINT NOT NULL DEFAULT 524288000, -- 500 MB default
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 2. Auth Sessions (Dual Auth: Cookie Web & Bearer Mobile/Ext)
CREATE TABLE IF NOT EXISTS auth_sessions (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash BYTEA NOT NULL UNIQUE,
    client_type VARCHAR(20) NOT NULL DEFAULT 'WEB',
    user_agent TEXT,
    ip_address INET,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_active_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_auth_sessions_user ON auth_sessions(user_id, expires_at);

-- 3. Folders (Adjacency List + Public Share)
CREATE TABLE IF NOT EXISTS folders (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    parent_id UUID REFERENCES folders(id) ON DELETE CASCADE,
    name VARCHAR(120) NOT NULL,
    description VARCHAR(500),
    position INT NOT NULL DEFAULT 0,
    share_token_hash BYTEA UNIQUE,
    share_enabled_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_folders_user ON folders(user_id, parent_id, position);

-- 4. Bookmarks & Notes
CREATE TABLE IF NOT EXISTS bookmarks (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    folder_id UUID REFERENCES folders(id) ON DELETE SET NULL,
    item_type VARCHAR(16) NOT NULL DEFAULT 'LINK',

    -- URL & Normalization
    raw_url TEXT,
    normalized_url TEXT,
    url_hash BYTEA,
    normalized_domain VARCHAR(255),

    -- Metadata
    title TEXT NOT NULL,
    description TEXT,
    site_name VARCHAR(120),
    thumbnail_url TEXT,
    favicon_url TEXT,
    user_edited_title BOOLEAN NOT NULL DEFAULT FALSE,
    user_edited_desc BOOLEAN NOT NULL DEFAULT FALSE,

    -- Notes & Optimistic Concurrency
    notes_plain TEXT,
    note_document JSONB,
    note_revision INT NOT NULL DEFAULT 0,
    last_note_save_id UUID,

    -- Organization & Flags
    is_pinned BOOLEAN NOT NULL DEFAULT FALSE,
    is_favorite BOOLEAN NOT NULL DEFAULT FALSE,
    is_archived BOOLEAN NOT NULL DEFAULT FALSE,
    position INT NOT NULL DEFAULT 0,

    -- Worker, Idempotency & Health
    client_request_id UUID,
    source VARCHAR(16) NOT NULL DEFAULT 'WEB',
    metadata_status VARCHAR(16) NOT NULL DEFAULT 'PENDING',
    metadata_error VARCHAR(300),
    last_enriched_at TIMESTAMPTZ,
    http_status_code SMALLINT,
    link_health VARCHAR(16) NOT NULL DEFAULT 'UNKNOWN',
    last_checked_at TIMESTAMPTZ,

    -- Search Vector
    search_vector TSVECTOR NOT NULL DEFAULT ''::tsvector,

    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at TIMESTAMPTZ,

    CONSTRAINT uq_user_client_req UNIQUE (user_id, client_request_id)
);

-- Partial Composite Indexes for Bookmarks
CREATE INDEX IF NOT EXISTS idx_bookmarks_main_feed
    ON bookmarks (user_id, is_archived, is_pinned DESC, created_at DESC, id DESC)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_bookmarks_folder_feed
    ON bookmarks (user_id, folder_id, is_pinned DESC, position ASC, created_at DESC, id DESC)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_bookmarks_favorites
    ON bookmarks (user_id, created_at DESC, id DESC)
    WHERE deleted_at IS NULL AND is_favorite = TRUE;

CREATE INDEX IF NOT EXISTS idx_bookmarks_domain
    ON bookmarks (user_id, normalized_domain, created_at DESC)
    WHERE deleted_at IS NULL AND normalized_domain IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_bookmarks_url_hash
    ON bookmarks (user_id, url_hash)
    WHERE deleted_at IS NULL AND url_hash IS NOT NULL;

-- Full-Text & Trigram Search Indexes
CREATE INDEX IF NOT EXISTS idx_bookmarks_search_vector
    ON bookmarks USING GIN (search_vector)
    WHERE deleted_at IS NULL;

CREATE INDEX IF NOT EXISTS idx_bookmarks_title_trgm
    ON bookmarks USING GIN (title gin_trgm_ops)
    WHERE deleted_at IS NULL;

-- 5. Tags & Tag Junction
CREATE TABLE IF NOT EXISTS tags (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    slug VARCHAR(64) NOT NULL,
    name VARCHAR(64) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT uq_user_tag_slug UNIQUE (user_id, slug)
);

CREATE TABLE IF NOT EXISTS bookmark_tags (
    bookmark_id UUID NOT NULL REFERENCES bookmarks(id) ON DELETE CASCADE,
    tag_id UUID NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (bookmark_id, tag_id)
);

CREATE INDEX IF NOT EXISTS idx_bookmark_tags_reverse ON bookmark_tags (tag_id, bookmark_id);

-- 6. Media Assets (Cloudflare R2 Tracking)
CREATE TABLE IF NOT EXISTS media_assets (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    bookmark_id UUID REFERENCES bookmarks(id) ON DELETE SET NULL,
    object_key TEXT NOT NULL UNIQUE,
    mime_type VARCHAR(100) NOT NULL,
    size_bytes BIGINT NOT NULL,
    status VARCHAR(16) NOT NULL DEFAULT 'PENDING_UPLOAD',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_media_assets_user ON media_assets (user_id, status);

-- 7. Background Tasks
CREATE TABLE IF NOT EXISTS background_tasks (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind VARCHAR(24) NOT NULL,
    status VARCHAR(16) NOT NULL DEFAULT 'PENDING',
    total_items INT NOT NULL DEFAULT 0,
    processed_items INT NOT NULL DEFAULT 0,
    failed_items INT NOT NULL DEFAULT 0,
    error_message VARCHAR(300),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    finished_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_background_tasks_user ON background_tasks (user_id, created_at DESC);

-- 8. Trigger for Search Vector Generation
CREATE OR REPLACE FUNCTION bookmarks_search_vector_update() RETURNS trigger AS $$
BEGIN
  NEW.search_vector :=
    setweight(to_tsvector('simple', COALESCE(NEW.title, '')), 'A') ||
    setweight(to_tsvector('simple', COALESCE(NEW.normalized_domain, '') || ' ' || COALESCE(NEW.raw_url, '')), 'B') ||
    setweight(to_tsvector('simple', COALESCE(NEW.description, '') || ' ' || COALESCE(NEW.notes_plain, '')), 'C');
  RETURN NEW;
END
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_bookmarks_search_vector_update ON bookmarks;
CREATE TRIGGER trg_bookmarks_search_vector_update
BEFORE INSERT OR UPDATE OF title, description, notes_plain, normalized_domain, raw_url
ON bookmarks
FOR EACH ROW
EXECUTE FUNCTION bookmarks_search_vector_update();
