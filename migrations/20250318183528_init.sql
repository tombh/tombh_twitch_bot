-- Add migration script here
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS mate (
		id          INTEGER  PRIMARY KEY AUTOINCREMENT,
		name        TEXT     UNIQUE  NOT NULL,
		last_played DATETIME
);

CREATE TABLE IF NOT EXISTS achievement (
	  achiever INTEGER NOT NULL,
		achievement TEXT,
		timestamp DATETIME DEFAULT (datetime('now', 'utc')),
		data TEXT,
		FOREIGN KEY(achiever) REFERENCES mate(id)
);
