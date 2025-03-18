use color_eyre::Result;

impl crate::Bot {
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

        let path = format!("/home/streamer/Documents/{username}-arrived.mp3");
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
}
