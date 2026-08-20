-- "Delete for me": hides a message from a single account's history only (the
-- other side still sees it). "Delete for everyone" continues to use messages.deleted_at.
CREATE TABLE hidden_messages (
    message_id UUID NOT NULL,
    account_id UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    PRIMARY KEY (message_id, account_id)
);
CREATE INDEX hidden_messages_account_idx ON hidden_messages(account_id);
