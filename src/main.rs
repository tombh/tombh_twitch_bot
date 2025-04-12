pub mod commands;
pub mod database;
pub mod websocket;

use std::sync::Arc;

use clap::Parser;
use color_eyre::Result;
use eyre::{ContextCompat as _, WrapErr as _};
use std::io::Write as _;
use tokio::sync::Mutex;
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _};
use twitch_api::{
    client::ClientDefault,
    eventsub::{self, Event, Message, Payload},
    HelixClient,
};
use twitch_oauth2::{Scope, TwitchToken as _};

pub const BROADCASTER_ID: &str = "630634223";
const BOT_ID: &str = "630634223";
const TWITCH_CLI_ENV_PATH: &str = "/home/streamer/.config/tbhbot/.env";

#[derive(Parser, Debug, Clone)]
#[clap(about, version)]
pub struct Cli {
    /// Client ID of twitch application
    #[clap(long, action)]
    pub get_new_token: bool,
    /// Path to config file
    #[clap(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/config.toml"))]
    pub config: std::path::PathBuf,
    /// Mock websocket server for testing
    #[clap(long)]
    pub ws_server: Option<url::Url>,
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
    let mut is_restart = false;
    color_eyre::install()?;
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    dotenvy::from_path(TWITCH_CLI_ENV_PATH).expect("Couldn't load .env file");
    for _ in 0..100 {
        let result = initialise(is_restart).await;
        if let Err(error) = result {
            tracing::error!("App crashed: {error:?}");
        }
        is_restart = true;
        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
        tracing::info!("Restarting bot");
    }

    Ok(())
}

async fn initialise(is_restart: bool) -> Result<(), eyre::Report> {
    let cli_args = Cli::parse();
    let config = Config::load(&cli_args.config)?;

    let client: HelixClient<reqwest::Client> = twitch_api::HelixClient::with_client(
        ClientDefault::default_client_with_name(Some("tombh_chatbot".parse()?))?,
    );

    let user = std::env::var("USER").expect("No value in `$USER` ENV var");
    let state_directory = std::path::PathBuf::from(format!("/home/{user}/.local/state/tbhbot"));
    let access_token_path = state_directory.join("access.token");
    let refresh_token_path = state_directory.join("refresh.token");

    let client_secret_string =
        std::env::var("CLIENTSECRET").expect("Couldn't find CLIENTSECRET in the environment");
    let client_secret = twitch_oauth2::ClientSecret::new(client_secret_string);

    let token = if !cli_args.get_new_token || is_restart {
        let mut access_token_string = std::fs::read_to_string(access_token_path)?;
        access_token_string = access_token_string.trim().to_string();
        let access_token = twitch_oauth2::AccessToken::from(access_token_string);

        let mut refresh_token_string = std::fs::read_to_string(refresh_token_path)?;
        refresh_token_string = refresh_token_string.trim().to_string();
        let refresh_token = twitch_oauth2::RefreshToken::from(refresh_token_string);

        twitch_oauth2::UserToken::from_existing(
            &client,
            access_token,
            Some(refresh_token),
            Some(client_secret),
        )
        .await?
    } else {
        let client_id_string =
            std::env::var("CLIENTID").expect("Couldn't find CLIENTID in the environment");
        let mut builder = twitch_oauth2::tokens::DeviceUserTokenBuilder::new(
            client_id_string,
            [
                Scope::UserReadChat,
                Scope::UserWriteChat,
                Scope::ModeratorReadFollowers,
            ]
            .to_vec(),
        );
        let code = builder.start(&client).await?;
        println!("Please go to: {}", code.verification_uri);
        let mut token = builder.wait_for_code(&client, tokio::time::sleep).await?;

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

        token.set_secret(Some(client_secret));

        token
    };

    let Some(twitch_api::helix::users::User {
        id: broadcaster, ..
    }) = client.get_user_from_id(BROADCASTER_ID, &token).await?
    else {
        eyre::bail!("No broadcaster found with ID: {}", BROADCASTER_ID);
    };
    let token = Arc::new(Mutex::new(token));

    let bot = Bot {
        db: database::Database::new().await?,
        opts: cli_args,
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
        let connect_url = match self.opts.ws_server.clone() {
            Some(uri) => uri,
            None => twitch_api::TWITCH_EVENTSUB_WEBSOCKET_URL.clone(),
        };

        // To make a connection to the chat we need to use a websocket connection.
        // This is a wrapper for the websocket connection that handles the reconnects and handles all messages from eventsub.
        let websocket = websocket::ChatWebsocketClient {
            session_id: None,
            token: self.token.clone(),
            client: self.client.clone(),
            connect_url,
            chats: vec![self.broadcaster.clone()],
        };
        let token_refresher = async move {
            let token = self.token.clone();
            let client: HelixClient<reqwest::Client> = twitch_api::HelixClient::with_client(
                ClientDefault::default_client_with_name(Some("tombh_chatbot".parse()?))?,
            );
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
        let eventer = websocket.run(|event, timestamp| async {
            let result = self.handle_event(event, timestamp).await;
            if let Err(error) = result {
                tracing::error!("Handling event: {error:?}");
            }
            Ok(())
        });

        futures::future::try_join(eventer, token_refresher).await?;
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

                self.db.save_message(&payload, timestamp).await?;

                if let Some(original) = payload.message.text.strip_prefix("!") {
                    let mut split_whitespace = original.split_whitespace();

                    let command = split_whitespace.next().unwrap();

                    let maybe_more = original.split_once(char::is_whitespace);
                    let arguments = maybe_more.map(|more| more.1);

                    self.command(&payload, command, arguments).await?;
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
            Event::ChannelFollowV2(Payload {
                message: Message::Notification(payload),
                ..
            }) => self.new_follower(&payload)?,
            Event::ChannelRaidV1(Payload {
                message: Message::Notification(payload),
                ..
            }) => self.incoming_raid(&payload)?,

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
        rest: Option<&str>,
    ) -> Result<(), eyre::Report> {
        tracing::info!("Command: {}", command);
        let username = payload.chatter_user_name.as_str();

        match command {
            "arrived" => self.arrived(payload, username).await?,
            "chirp" => self.chirp(payload, username, None).await?,
            "osd" => self.osd(payload, rest).await?,
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

    fn onscreen_popup(message: String, category: &str) -> Result<()> {
        std::process::Command::new("notify-send")
            .arg(format!("--category={}", category))
            .arg(message)
            .spawn()?;
        Ok(())
    }

    fn new_follower(&self, payload: &eventsub::channel::ChannelFollowV2Payload) -> Result<()> {
        tracing::info!("New follower: {payload:?}");
        let message = format!(" \nWelcome {} ❤️", payload.user_name);
        Self::onscreen_popup(message, "twitch-new-follower")?;

        let path = "/home/streamer/Documents/great_scott.mp3";
        std::process::Command::new("mpv")
            .arg("--volume=50")
            .arg(path)
            .spawn()?;
        Ok(())
    }

    fn incoming_raid(&self, payload: &eventsub::channel::ChannelRaidV1Payload) -> Result<()> {
        tracing::info!("Raid: {payload:?}");
        let message = format!(
            " \n{} RAIDERS FROM {}!",
            payload.viewers, payload.from_broadcaster_user_name
        );
        Self::onscreen_popup(message, "twitch-raid")?;

        let path = "/home/streamer/Documents/hand_of_god.mp3";
        std::process::Command::new("mpv")
            .arg("--volume=50")
            .arg(path)
            .spawn()?;
        Ok(())
    }
}
