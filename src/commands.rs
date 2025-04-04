use color_eyre::Result;
use eyre::ContextCompat as _;
use rand::{seq::IteratorRandom as _, Rng as _};

impl crate::Bot {
    pub async fn text_responder(
        &self,
        command: &str,
        message: &crate::eventsub::channel::ChannelChatMessageV1Payload,
    ) -> Result<()> {
        if let Some(response) = self.config.command.iter().find(|c| c.trigger == command) {
            self.send_message_reply(
                &message.message_id,
                response
                    .response
                    .replace("{user}", message.chatter_user_name.as_str())
                    .as_str(),
            )
            .await?;
        }

        Ok(())
    }

    pub async fn arrived(
        &self,
        payload: &crate::eventsub::channel::ChannelChatMessageV1Payload,
        username: &str,
    ) -> Result<()> {
        let mate = self.db.get_mate(username).await?;
        let elapsed = chrono::Utc::now() - mate.last_played;
        if elapsed.num_hours() < 12 {
            tracing::info!("Not playing {username}'s sound. {elapsed} to go.");
            let message = format!("You're already here {username}!");
            self.send_message_reply(&payload.message_id, message.as_str())
                .await?;
            return Ok(());
        }

        let path = format!("/home/streamer/Documents/arrivals/{username}-arrived.mp3");
        if !std::path::Path::new(&path).exists() {
            let message = "You don't have an arrival sound yet, type \"!sounds\" to find out how.";
            self.send_message_reply(&payload.message_id, message)
                .await?;
            return Ok(());
        }

        std::process::Command::new("mpv")
            .arg("--volume=50")
            .arg(path)
            .spawn()?;

        let message = format!("{username} has arrived 📣");
        self.send_message_reply(&payload.message_id, message.as_str())
            .await?;

        self.db.set_last_played(username).await?;
        Ok(())
    }

    pub async fn chirp(
        &self,
        payload: &crate::eventsub::channel::ChannelChatMessageV1Payload,
        username: &str,
        depth: Option<u16>,
    ) -> Result<()> {
        let chicken_chance = 0.05;
        let mut rng = rand::rng();
        let mut repeats = depth.unwrap_or_default();

        let directory = "/home/streamer/Documents/chirps";
        let chirps = std::fs::read_dir(directory)?;
        let mut sound = chirps.choose(&mut rng).context("No chirp found")??.path();
        let chicken_path = "/home/streamer/Documents/rubber-chicken.mp3";

        let is_chicken = rng.random_bool(chicken_chance);
        if is_chicken {
            sound = std::path::Path::new(chicken_path).to_path_buf();
            repeats += 1;
        }

        if repeats > 0 && !is_chicken {
            self.chicken_run_end(username, repeats, payload).await?;
            return Ok(());
        }

        let mut process = std::process::Command::new("mpv")
            .arg("--volume=50")
            .arg(sound)
            .spawn()?;

        if is_chicken {
            if rng.random_bool(chicken_chance) {
                process.wait()?;
                std::boxed::Box::pin(self.chirp(payload, username, Some(repeats))).await?;
                return Ok(());
            } else {
                self.chicken_run_end(username, repeats, payload).await?;
            }
        }
        Ok(())
    }

    async fn chicken_run_end(
        &self,
        username: &str,
        repeats: u16,
        payload: &crate::eventsub::channel::ChannelChatMessageV1Payload,
    ) -> Result<()> {
        tracing::info!("{username} got {repeats} chickens");
        if repeats > 1 {
            let message = format!("Wooooah that's {repeats} rubber chickens!");
            self.send_message_reply(&payload.message_id, message.as_str())
                .await?;
            let mate = self.db.get_mate(username).await?;
            let achievement = crate::database::Achievement {
                achiever: mate.id,
                kind: crate::database::AchievementKind::ChickenRun,
                data: serde_json::json!({
                    "repeats": repeats,
                }),
                timestamp: chrono::Utc::now(),
            };
            self.db.add_achievement(achievement).await?;
        }
        Ok(())
    }

    pub async fn osd(
        &self,
        payload: &crate::eventsub::channel::ChannelChatMessageV1Payload,
        arguments: Option<&str>,
    ) -> Result<()> {
        if let Some(text) = arguments {
            let message = format!(" {} says: {text}", payload.chatter_user_name);
            Self::onscreen_popup(message, "twitch-osd")?;
        };
        Ok(())
    }
}
