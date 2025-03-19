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
        let ws =
            websocket.run(|event, timestamp| async { self.handle_event(event, timestamp).await });
        futures::future::try_join(ws, refresh_token).await?;
        Ok(())
    }

    async fn handle_event(
        &self,
        event: Event,
        timestamp: twitch_api::types::Timestamp,
    ) -> Result<(), eyre::Report> {
        match event {
            // The `channel.chat.message` subscription type sends a notification when
            // any user sends a message to a channel’s chat room.
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
                    let arguments = split_whitespace.next();

                    self.command(&payload, command, arguments).await?;
                } else {
                    self.db.save_message(&payload, timestamp).await?;
                }
            }
            // The `channel.chat.notification` subscription type sends a notification
            // when an event that appears in chat occurs, such as someone subscribing
            // to the channel or a subscription is gifted.
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
            Event::AutomodMessageHoldV1(payload) => Self::log_event(&payload),
            Event::AutomodMessageHoldV2(payload) => Self::log_event(&payload),
            Event::AutomodMessageUpdateV1(payload) => Self::log_event(&payload),
            Event::AutomodMessageUpdateV2(payload) => Self::log_event(&payload),
            Event::AutomodSettingsUpdateV1(payload) => Self::log_event(&payload),
            Event::AutomodTermsUpdateV1(payload) => Self::log_event(&payload),
            Event::ChannelAdBreakBeginV1(payload) => Self::log_event(&payload),
            Event::ChannelChatClearV1(payload) => Self::log_event(&payload),
            Event::ChannelChatClearUserMessagesV1(payload) => Self::log_event(&payload),
            Event::ChannelChatMessageDeleteV1(payload) => Self::log_event(&payload),
            Event::ChannelChatUserMessageHoldV1(payload) => Self::log_event(&payload),
            Event::ChannelChatUserMessageUpdateV1(payload) => Self::log_event(&payload),
            Event::ChannelChatSettingsUpdateV1(payload) => Self::log_event(&payload),
            Event::ChannelCharityCampaignDonateV1(payload) => Self::log_event(&payload),
            Event::ChannelCharityCampaignProgressV1(payload) => Self::log_event(&payload),
            Event::ChannelCharityCampaignStartV1(payload) => Self::log_event(&payload),
            Event::ChannelCharityCampaignStopV1(payload) => Self::log_event(&payload),
            Event::ChannelUpdateV2(payload) => Self::log_event(&payload),
            Event::ChannelFollowV2(payload) => Self::log_event(&payload),
            Event::ChannelSubscribeV1(payload) => Self::log_event(&payload),
            Event::ChannelCheerV1(payload) => Self::log_event(&payload),
            Event::ChannelBanV1(payload) => Self::log_event(&payload),
            Event::ChannelUnbanV1(payload) => Self::log_event(&payload),
            Event::ChannelUnbanRequestCreateV1(payload) => Self::log_event(&payload),
            Event::ChannelUnbanRequestResolveV1(payload) => Self::log_event(&payload),
            Event::ChannelVipAddV1(payload) => Self::log_event(&payload),
            Event::ChannelVipRemoveV1(payload) => Self::log_event(&payload),
            Event::ChannelWarningAcknowledgeV1(payload) => Self::log_event(&payload),
            Event::ChannelWarningSendV1(payload) => Self::log_event(&payload),
            Event::ChannelPointsAutomaticRewardRedemptionAddV1(payload) => {
                Self::log_event(&payload)
            }
            Event::ChannelPointsCustomRewardAddV1(payload) => Self::log_event(&payload),
            Event::ChannelPointsCustomRewardUpdateV1(payload) => Self::log_event(&payload),
            Event::ChannelPointsCustomRewardRemoveV1(payload) => Self::log_event(&payload),
            Event::ChannelPointsCustomRewardRedemptionAddV1(payload) => Self::log_event(&payload),
            Event::ChannelPointsCustomRewardRedemptionUpdateV1(payload) => {
                Self::log_event(&payload)
            }
            Event::ChannelPollBeginV1(payload) => Self::log_event(&payload),
            Event::ChannelPollProgressV1(payload) => Self::log_event(&payload),
            Event::ChannelPollEndV1(payload) => Self::log_event(&payload),
            Event::ChannelPredictionBeginV1(payload) => Self::log_event(&payload),
            Event::ChannelPredictionProgressV1(payload) => Self::log_event(&payload),
            Event::ChannelPredictionLockV1(payload) => Self::log_event(&payload),
            Event::ChannelPredictionEndV1(payload) => Self::log_event(&payload),
            Event::ChannelRaidV1(payload) => Self::log_event(&payload),
            Event::ChannelSharedChatBeginV1(payload) => Self::log_event(&payload),
            Event::ChannelSharedChatEndV1(payload) => Self::log_event(&payload),
            Event::ChannelSharedChatUpdateV1(payload) => Self::log_event(&payload),
            Event::ChannelShieldModeBeginV1(payload) => Self::log_event(&payload),
            Event::ChannelShieldModeEndV1(payload) => Self::log_event(&payload),
            Event::ChannelShoutoutCreateV1(payload) => Self::log_event(&payload),
            Event::ChannelShoutoutReceiveV1(payload) => Self::log_event(&payload),
            Event::ChannelSuspiciousUserMessageV1(payload) => Self::log_event(&payload),
            Event::ChannelSuspiciousUserUpdateV1(payload) => Self::log_event(&payload),
            Event::ChannelGoalBeginV1(payload) => Self::log_event(&payload),
            Event::ChannelGoalProgressV1(payload) => Self::log_event(&payload),
            Event::ChannelGoalEndV1(payload) => Self::log_event(&payload),
            Event::ChannelHypeTrainBeginV1(payload) => Self::log_event(&payload),
            Event::ChannelHypeTrainProgressV1(payload) => Self::log_event(&payload),
            Event::ChannelHypeTrainEndV1(payload) => Self::log_event(&payload),
            Event::ChannelModerateV1(payload) => Self::log_event(&payload),
            Event::ChannelModerateV2(payload) => Self::log_event(&payload),
            Event::ChannelModeratorAddV1(payload) => Self::log_event(&payload),
            Event::ChannelModeratorRemoveV1(payload) => Self::log_event(&payload),
            Event::ConduitShardDisabledV1(payload) => Self::log_event(&payload),
            Event::StreamOnlineV1(payload) => Self::log_event(&payload),
            Event::StreamOfflineV1(payload) => Self::log_event(&payload),
            Event::UserUpdateV1(payload) => Self::log_event(&payload),
            Event::UserAuthorizationGrantV1(payload) => Self::log_event(&payload),
            Event::UserAuthorizationRevokeV1(payload) => Self::log_event(&payload),
            Event::UserWhisperMessageV1(payload) => Self::log_event(&payload),
            Event::ChannelSubscriptionEndV1(payload) => Self::log_event(&payload),
            Event::ChannelSubscriptionGiftV1(payload) => Self::log_event(&payload),
            Event::ChannelSubscriptionMessageV1(payload) => Self::log_event(&payload),
            _ => tracing::warn!("Uknown Twitch event"),
        }
        Ok(())
    }

    fn log_event<T: eventsub::EventSubscription>(event: &Payload<T>) {
        tracing::info!("{}", event.get_event_type());
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
            "arrived" => self.arrived(payload, username).await?,
            "chirp" => self.chirp(payload, username, None).await?,
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
