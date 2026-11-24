-- Keep the invite token readable for the folder owner so the link can be shown again,
-- like folders.share_token for public links. Lookups still go through token_hash.
-- Links created before this migration have no stored token; the owner can make a new one.
ALTER TABLE folder_invitations ADD COLUMN IF NOT EXISTS token TEXT;
