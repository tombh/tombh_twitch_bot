use color_eyre::Result;

impl crate::Bot {
    pub fn arrived(&self, username: &str) -> Result<()> {
        let path = format!("/home/streamer/Documents/{username}-arrived.mp3");
        std::process::Command::new("mpv")
            .arg("--volume=50")
            .arg(path)
            .spawn()?;
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
