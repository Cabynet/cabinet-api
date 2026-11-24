-- Optional expiry for public share links and invite links. NULL never expires;
-- an expired link behaves exactly like one that was turned off.
ALTER TABLE folders ADD COLUMN IF NOT EXISTS share_expires_at TIMESTAMPTZ;
ALTER TABLE folder_invitations ADD COLUMN IF NOT EXISTS expires_at TIMESTAMPTZ;
