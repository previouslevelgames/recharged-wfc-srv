-- Messages between two players who keep each other as friends
-- (docs/profiles.md, "Chat"), kept for 30 days.
CREATE TABLE chat_messages (
    message_id BIGINT      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    sender     BIGINT      NOT NULL REFERENCES players ON DELETE CASCADE,
    recipient  BIGINT      NOT NULL REFERENCES players ON DELETE CASCADE,
    body       TEXT        NOT NULL CHECK (char_length(body) BETWEEN 1 AND 300),
    sent_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- When the recipient's launcher showed it.
    read_at    TIMESTAMPTZ,
    CHECK (sender <> recipient)
);
CREATE INDEX chat_messages_by_pair
    ON chat_messages (least(sender, recipient), greatest(sender, recipient), message_id);
CREATE INDEX chat_messages_unread ON chat_messages (recipient, sender) WHERE read_at IS NULL;
CREATE INDEX chat_messages_by_time ON chat_messages (sent_at);
