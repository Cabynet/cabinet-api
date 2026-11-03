-- Additive migration; existing items and sessions are preserved.
ALTER TABLE bookmarks ADD COLUMN IF NOT EXISTS content_type VARCHAR(32);
ALTER TABLE bookmarks ADD COLUMN IF NOT EXISTS drawing JSONB;
ALTER TABLE bookmarks ADD COLUMN IF NOT EXISTS metadata_attempts INT NOT NULL DEFAULT 0;
ALTER TABLE bookmarks ADD COLUMN IF NOT EXISTS metadata_started_at TIMESTAMPTZ;
ALTER TABLE folders ADD COLUMN IF NOT EXISTS share_token TEXT;

CREATE TABLE IF NOT EXISTS folder_members (
    folder_id UUID NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role VARCHAR(16) NOT NULL CHECK (role IN ('VIEWER', 'CONTRIBUTOR', 'EDITOR')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (folder_id, user_id)
);
CREATE TABLE IF NOT EXISTS folder_invitations (
    folder_id UUID PRIMARY KEY REFERENCES folders(id) ON DELETE CASCADE,
    token_hash BYTEA NOT NULL UNIQUE,
    role VARCHAR(16) NOT NULL CHECK (role IN ('VIEWER', 'CONTRIBUTOR', 'EDITOR')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TABLE IF NOT EXISTS oauth_accounts (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider VARCHAR(16) NOT NULL,
    provider_account_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (provider, provider_account_id),
    UNIQUE (user_id, provider)
);
CREATE INDEX IF NOT EXISTS idx_bookmarks_metadata_recovery ON bookmarks (metadata_status, metadata_started_at)
    WHERE deleted_at IS NULL AND raw_url IS NOT NULL;
