pub const SOCKET_PATH: &str = "/tmp/tattoy-twitch.sock";

#[derive(serde::Deserialize, serde::Serialize, Debug)]
pub struct BotMessage {
    pub username: String,
    pub regexish: String,
    pub emote: String,
}
