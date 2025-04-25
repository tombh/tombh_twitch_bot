pub mod bot;
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
    eventsub::{self},
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
    pub trigger: Vec<String>,
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
    let config = Config::load(&workspace_dir().join("config.toml"))?;

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

    let socket = tokio::net::UnixStream::connect(tattoy_twitch_tombh_plugin::SOCKET_PATH)
        .await
        .unwrap();
    let tattoy_socket = Arc::new(Mutex::new(socket));

    let bot = bot::Bot {
        db: database::Database::new().await?,
        opts: cli_args,
        client,
        token,
        config,
        broadcaster,
        tattoy_socket,
    };
    bot.start().await?;
    Ok(())
}

#[inline]
pub fn workspace_dir() -> std::path::PathBuf {
    let output = std::process::Command::new(env!("CARGO"))
        .arg("locate-project")
        .arg("--workspace")
        .arg("--message-format=plain")
        .output()
        .unwrap()
        .stdout;
    let cargo_path = std::path::Path::new(std::str::from_utf8(&output).unwrap().trim());
    let workspace_dir = cargo_path.parent().unwrap().to_path_buf();
    tracing::debug!("Using workspace directory: {workspace_dir:?}");
    workspace_dir
}
