pub mod renderer;
pub mod utils;

use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> color_eyre::eyre::Result<()> {
    let (messages_tx, messages_rx) = tokio::sync::mpsc::channel(16);
    setup_logging()?;
    start_listener(messages_tx);
    renderer::Plugin::start(messages_rx).await
}

/// Start a dedicated thread for the input listener.
fn start_listener(sender: tokio::sync::mpsc::Sender<tattoy_protocol::PluginInputMessages>) {
    std::thread::spawn(move || {
        let result = listen_for_tattoy_messages(&sender);
        if let Err(error) = result {
            tracing::error!("Error parsing JSON input: {error:?}");
        }
    });
}

/// Listen for JSON messages from Tattoy. Sent over STDIN.
fn listen_for_tattoy_messages(
    sender: &tokio::sync::mpsc::Sender<tattoy_protocol::PluginInputMessages>,
) -> color_eyre::eyre::Result<()> {
    tracing::debug!("Starting to listen on STDIN for messages from Tattoy");
    for maybe_line in std::io::stdin().lines() {
        let message: tattoy_protocol::PluginInputMessages =
            serde_json::from_str(maybe_line?.as_str())?;
        sender.blocking_send(message)?;
    }
    Ok(())
}

/// Setup logging to a file.
fn setup_logging() -> color_eyre::eyre::Result<()> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open("/tmp/tattoy-twitch.log")?;
    let file_appender = tracing_subscriber::fmt::layer().with_writer(file);
    tracing_subscriber::registry().with(file_appender).init();

    Ok(())
}
