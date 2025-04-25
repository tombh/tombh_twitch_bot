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

#[derive(Debug, sqlx::Type)]
pub enum AchievementKind {
    ChickenRun,
}

#[derive(Debug, sqlx::FromRow)]
pub struct Message {
    pub user_id: i64,
    pub user_name: String,
    pub text: String,
    pub kind: twitch_api::eventsub::channel::chat::message::MessageType,
}

#[derive(Debug, sqlx::FromRow)]
pub struct Achievement {
    pub achiever: i32,
    pub kind: AchievementKind,
    pub data: serde_json::Value,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

pub struct Database {
    connection: sqlx::SqlitePool,
}

impl Database {
    pub async fn new() -> Result<Self> {
        let db = Self {
            connection: sqlx::SqlitePool::connect(format!("sqlite://{}", DB_PATH).as_str()).await?,
        };

        Ok(db)
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

    pub async fn add_achievement(&self, achievement: Achievement) -> Result<()> {
        self.connection
            .execute(
                sqlx::query(
                    "
                    INSERT INTO achievement(achiever, achievement, data)
                    VALUES (?, ?, ?);
                    ",
                )
                .bind(achievement.achiever)
                .bind(achievement.kind)
                .bind(achievement.data),
            )
            .await?;

        Ok(())
    }

    pub async fn save_message(
        &self,
        payload: &crate::eventsub::channel::ChannelChatMessageV1Payload,
        timestamp: twitch_api::types::Timestamp,
    ) -> Result<()> {
        self.connection
            .execute(
                sqlx::query(
                    "
                    INSERT INTO message(twitch_user_id, timestamp, username, text, kind)
                    VALUES (?, ?, ?, ?, ?);
                    ",
                )
                .bind(payload.chatter_user_id.as_str())
                .bind(timestamp.as_str())
                .bind(payload.chatter_user_name.as_str())
                .bind(payload.message.text.clone())
                .bind(serde_json::to_string(&payload.message_type)?),
            )
            .await?;
        Ok(())
    }
}
