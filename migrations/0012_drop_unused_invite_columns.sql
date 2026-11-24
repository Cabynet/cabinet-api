-- Invite links can no longer be created or edited (folders use one General access link,
-- see 0011), so the readable token and expiry added in 0009/0010 are unused.
-- Dropping the token also removes stored raw secrets; lookups use token_hash.
ALTER TABLE folder_invitations DROP COLUMN IF EXISTS token, DROP COLUMN IF EXISTS expires_at;
