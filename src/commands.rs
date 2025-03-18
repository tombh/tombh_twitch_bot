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
        let directory = "/home/streamer/Documents/chirps";
        let chirps = std::fs::read_dir(directory)?;
        let mut rng = rand::rng();
        let is_chicken = rng.random_bool(chicken_chance);
        let sound = if is_chicken {
            std::path::Path::new("/home/streamer/Documents/rubber-chicken.mp3").to_path_buf()
        } else if depth.is_none() {
            chirps.choose(&mut rng).context("No chirp found")??.path()
        } else {
            let repeats = depth.unwrap_or_default();
            if repeats > 1 {
                let message = format!("Wooooah that's {repeats} rubber chickens!");
                self.send_message_reply(&payload.message_id, message.as_str())
                    .await?;
            }
            return Ok(());
        };
        let mut process = std::process::Command::new("mpv")
            .arg("--volume=50")
            .arg(sound)
            .spawn()?;

        if is_chicken && rng.random_bool(chicken_chance) {
            let repeats = depth.unwrap_or_default() + 1;

            process.wait()?;
            std::boxed::Box::pin(self.chirp(payload, username, Some(repeats))).await?;
        }
        Ok(())
    }
}
