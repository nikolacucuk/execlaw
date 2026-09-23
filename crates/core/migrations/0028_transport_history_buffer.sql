ALTER TABLE message_archive_messages ADD COLUMN topic_keywords TEXT NOT NULL DEFAULT '';

DROP TRIGGER IF EXISTS message_archive_search_insert;
DROP TRIGGER IF EXISTS message_archive_search_delete;
DROP TRIGGER IF EXISTS message_archive_search_update;
DROP TABLE message_archive_search;
CREATE VIRTUAL TABLE message_archive_search USING fts5(
    archive_message_id UNINDEXED,
    archive_id UNINDEXED,
    body,
    sender_name,
    topic_keywords,
    tokenize = 'unicode61'
);

INSERT INTO message_archive_search(
    archive_message_id, archive_id, body, sender_name, topic_keywords
)
SELECT archive_message_id, archive_id, body, COALESCE(sender_name, ''), topic_keywords
FROM message_archive_messages;

CREATE TRIGGER message_archive_search_insert AFTER INSERT ON message_archive_messages BEGIN
    INSERT INTO message_archive_search(archive_message_id, archive_id, body, sender_name, topic_keywords)
    VALUES (new.archive_message_id, new.archive_id, new.body, COALESCE(new.sender_name, ''), new.topic_keywords);
END;

CREATE TRIGGER message_archive_search_delete AFTER DELETE ON message_archive_messages BEGIN
    DELETE FROM message_archive_search WHERE archive_message_id = old.archive_message_id;
END;

CREATE TRIGGER message_archive_search_update AFTER UPDATE OF body, sender_name, topic_keywords ON message_archive_messages BEGIN
    DELETE FROM message_archive_search WHERE archive_message_id = old.archive_message_id;
    INSERT INTO message_archive_search(archive_message_id, archive_id, body, sender_name, topic_keywords)
    VALUES (new.archive_message_id, new.archive_id, new.body, COALESCE(new.sender_name, ''), new.topic_keywords);
END;