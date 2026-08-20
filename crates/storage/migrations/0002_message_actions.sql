-- Message actions: reply, edit, delete.
--
-- reply_to   → the message this one quotes (plain UUID, not FK-constrained so a
--              deleted target doesn't cascade; the client resolves the preview).
-- edited_at  → set when the sender edits; drives the "edited" label.
-- deleted_at → soft-delete tombstone; deleted rows keep their seq (gap-free) but
--              are omitted from history and have their ciphertext cleared.
ALTER TABLE messages
    ADD COLUMN reply_to   UUID,
    ADD COLUMN edited_at  TIMESTAMPTZ,
    ADD COLUMN deleted_at TIMESTAMPTZ;
