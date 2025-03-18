use color_eyre::Result;
use sqlx::Executor as _;

// TODO: make a CLI arg for this
const DB_PATH: &str = "tbhbot.db";

#[derive(Debug, sqlx::FromRow)]
pub struct Mate {
    pub id: i32,
    pub name: String,
    pub last_played: chrono::DateTime<chrono::Utc>,
}

pub struct Database {
    connection: sqlx::SqlitePool,
}

impl Database {
    pub async fn new() -> Result<Self> {
        let mut db = Self {
            connection: sqlx::SqlitePool::connect(format!("sqlite://{}", DB_PATH).as_str()).await?,
        };
        db.setup().await?;
        Ok(db)
    }

    async fn setup(&mut self) -> Result<()> {
        self.connection
            .execute(
                "
                CREATE TABLE IF NOT EXISTS mate (
                    id          INTEGER  PRIMARY KEY AUTOINCREMENT,
                    name        TEXT     UNIQUE  NOT NULL,
                    last_played DATETIME
                )
                ",
            )
            .await?;
        Ok(())
    }

    pub async fn get_mate(&self, username: &str) -> Result<Mate> {
        self.connection
            .execute(
                sqlx::query(
                    "
                    INSERT INTO mate(name, last_played) VALUES (?, datetime('now', '-1 year'))
                    ON CONFLICT(name) DO NOTHING;
                    ",
                )
                .bind(username),
            )
            .await?;

        let mate = sqlx::query_as("SELECT * FROM mate WHERE name = ?")
            .bind(username)
            .fetch_one(&self.connection)
            .await?;

        Ok(mate)
    }

    pub async fn set_last_played(&self, username: &str) -> Result<()> {
        self.connection
            .execute(
                sqlx::query(
                    "
                    UPDATE mate SET last_played = ?
                    WHERE name = ?
                    ",
                )
                .bind(chrono::offset::Utc::now())
                .bind(username),
            )
            .await?;

        Ok(())
    }
}
