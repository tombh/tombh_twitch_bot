-- Add migration script here

CREATE TABLE IF NOT EXISTS message (
	  twitch_user_id INTEGER NOT NULL,
		timestamp DATETIME DEFAULT (datetime('now', 'utc')),
		username TEXT,
		text TEXT,
		kind TEXT
);
