pub mod commands;
pub mod database;
pub mod websocket;

use std::sync::Arc;

use clap::Parser;
use color_eyre::Result;
use eyre::{ContextCompat as _, WrapErr as _};
use std::io::Write as _;
use tokio::sync::Mutex;
use twitch_api::{
    client::ClientDefault,
    eventsub::{self, Event, Message, Payload},
    HelixClient,
};
use twitch_oauth2::{Scope, TwitchToken as _};

const BROADCASTER_ID: &str = "630634223";
const BOT_ID: &str = "630634223";

#[derive(Parser, Debug, Clone)]
#[clap(about, version)]
pub struct Cli {
    /// Client ID of twitch application
    #[clap(long, env, hide_env = true)]
    pub client_id: Option<twitch_oauth2::ClientId>,
    #[clap(long, env, hide_env = true)]
    pub broadcaster_login: twitch_api::types::UserName,
    /// Path to config file
    #[clap(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/config.toml"))]
    pub config: std::path::PathBuf,
}

#[derive(serde_derive::Serialize, serde_derive::Deserialize, Debug)]
pub struct Config {
    command: Vec<Command>,
}

#[derive(serde_derive::Serialize, serde_derive::Deserialize, Debug)]
pub struct Command {
    pub trigger: String,
    pub response: String,
}

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Self, eyre::Report> {
        let config = std::fs::read_to_string(path)?;
        toml::from_str(&config).wrap_err("Failed to parse config")
    }
}

#[tokio::main]
async fn main() -> Result<(), eyre::Report> {
    initialise().await
}

async fn initialise() -> Result<(), eyre::Report> {
    color_eyre::install()?;
    tracing_subscriber::fmt::fmt()
        .with_writer(std::io::stderr)
        .init();
    _ = dotenvy::dotenv();
    let opts = Cli::parse();
    let config = Config::load(&opts.config)?;

    let client: HelixClient<reqwest::Client> = twitch_api::HelixClient::with_client(
        ClientDefault::default_client_with_name(Some("my_chatbot".parse()?))?,
    );

    let user = std::env::var("USER").expect("No value in `$USER` ENV var");
    let state_directory = std::path::PathBuf::from(format!("/home/{user}/.local/state/tbhbot"));
    let access_token_path = state_directory.join("access.token");
    let refresh_token_path = state_directory.join("refresh.token");

    let token = if access_token_path.exists() {
        let mut access_token_string = std::fs::read_to_string(access_token_path)?;
        access_token_string.retain(|c| !c.is_whitespace());
        let access_token = twitch_oauth2::AccessToken::from(access_token_string);
        twitch_oauth2::UserToken::from_existing(&client, access_token, None, None).await?
    } else {
        let client_id = opts
            .client_id
            .clone()
            .expect("No existing tokens found, please provide client ID");
        let mut builder = twitch_oauth2::tokens::DeviceUserTokenBuilder::new(
            client_id,
            vec![Scope::UserReadChat, Scope::UserWriteChat],
        );
        let code = builder.start(&client).await?;
        println!("Please go to: {}", code.verification_uri);
        let token = builder.wait_for_code(&client, tokio::time::sleep).await?;

        let mut access_token_file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(access_token_path)?;
        writeln!(access_token_file, "{}", token.token().secret())?;

        let refresh_token = token
            .refresh_token
            .clone()
            .context("Couldn't get refresh token")?;
        let mut refresh_token_file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(refresh_token_path)?;
        writeln!(refresh_token_file, "{}", refresh_token.secret())?;
        token
    };

    let Some(twitch_api::helix::users::User {
        id: broadcaster, ..
    }) = client
        .get_user_from_login(&opts.broadcaster_login, &token)
        .await?
    else {
        eyre::bail!(
            "No broadcaster found with login: {}",
            opts.broadcaster_login
        );
    };
    let token = Arc::new(Mutex::new(token));

    let bot = Bot {
        db: database::Database::new().await?,
        opts,
        client,
        token,
        config,
        broadcaster,
    };
    bot.start().await?;
    Ok(())
}

pub struct Bot {
    pub db: database::Database,
    pub opts: Cli,
    pub client: HelixClient<'static, reqwest::Client>,
    pub token: Arc<Mutex<twitch_oauth2::UserToken>>,
    pub config: Config,
    pub broadcaster: twitch_api::types::UserId,
}

impl Bot {
    pub async fn start(&self) -> Result<(), eyre::Report> {
        // To make a connection to the chat we need to use a websocket connection.
        // This is a wrapper for the websocket connection that handles the reconnects and handles all messages from eventsub.
        let websocket = websocket::ChatWebsocketClient {
            session_id: None,
            token: self.token.clone(),
            client: self.client.clone(),
            connect_url: twitch_api::TWITCH_EVENTSUB_WEBSOCKET_URL.clone(),
            chats: vec![self.broadcaster.clone()],
        };
        let refresh_token = async move {
            let token = self.token.clone();
            let client = self.client.clone();
            // We check constantly if the token is valid.
            // We also need to refresh the token if it's about to be expired.
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                let mut token = token.lock().await;
                if token.expires_in() < std::time::Duration::from_secs(60) {
                    token
                        .refresh_token(&self.client)
                        .await
                        .wrap_err("couldn't refresh token")?;
                }
                token
                    .validate_token(&client)
                    .await
                    .wrap_err("couldn't validate token")?;
            }
            #[allow(unreachable_code)]
            Ok(())
        };
        let ws = websocket.run(|e, ts| async { self.handle_event(e, ts).await });
        futures::future::try_join(ws, refresh_token).await?;
        Ok(())
    }

    async fn handle_event(
        &self,
        event: Event,
        timestamp: twitch_api::types::Timestamp,
    ) -> Result<(), eyre::Report> {
        match event {
            Event::ChannelChatMessageV1(Payload {
                message: Message::Notification(payload),
                ..
            }) => {
                println!(
                    "[{}] {}: {}",
                    timestamp, payload.chatter_user_name, payload.message.text
                );
                if let Some(command) = payload.message.text.strip_prefix("!") {
                    let mut split_whitespace = command.split_whitespace();
                    let command = split_whitespace.next().unwrap();
                    let rest = split_whitespace.next();

                    self.command(&payload, command, rest).await?;
                }
            }
            Event::ChannelChatNotificationV1(Payload {
                message: Message::Notification(payload),
                ..
            }) => {
                println!(
                    "[{}] {}: {}",
                    timestamp,
                    match &payload.chatter {
                        eventsub::channel::chat::notification::Chatter::Chatter {
                            chatter_user_name: user,
                            ..
                        } => user.as_str(),
                        _ => "anonymous",
                    },
                    payload.message.text
                );
            }
            _ => {}
        }
        Ok(())
    }

    async fn command(
        &self,
        payload: &eventsub::channel::ChannelChatMessageV1Payload,
        command: &str,
        _rest: Option<&str>,
    ) -> Result<(), eyre::Report> {
        tracing::info!("Command: {}", command);
        let username = payload.chatter_user_name.as_str();

        match command {
            "arrived" => self.arrived(username).await?,
            _ => self.text_responder(command, payload).await?,
        }

        Ok(())
    }

    async fn send_message_reply(
        &self,
        parent_message_id: &twitch_api::types::MsgId,
        message: &str,
    ) -> Result<()> {
        let token = self.token.lock().await.clone();
        self.client
            .send_chat_message_reply(BROADCASTER_ID, BOT_ID, parent_message_id, message, &token)
            .await?;

        Ok(())
    }
}
