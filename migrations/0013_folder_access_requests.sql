-- "Request access", like Google Drive: someone without access asks, the owner approves with a role or denies.
CREATE TABLE IF NOT EXISTS folder_access_requests (
    folder_id UUID NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    message TEXT CHECK (char_length(message) <= 500),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (folder_id, user_id)
);
