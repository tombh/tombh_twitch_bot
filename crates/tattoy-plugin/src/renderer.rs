use color_eyre::eyre::Result;
use image::GenericImageView as _;
use std::io::Write as _;
use tokio::io::AsyncBufReadExt;

/// The number of microseconds in a second.
pub const ONE_MICROSECOND: u64 = 1_000_000;

/// The target frame rate for renders sent to Tattoy.
pub const TARGET_FRAME_RATE: u64 = 30;

/// The current state of the Tattoy user's terminal.
struct TTY {
    /// The size of the user's terminal.
    size: (u16, u16),
    /// The current position of the cursor in the user's terminal.
    cursor_position: (u16, u16),
    /// The contens of the terminal's cells. Characters and colour values.
    cells: Vec<tattoy_protocol::Cell>,
}

#[derive(Clone, Debug)]
struct ActiveEmote {
    regexish: String,
    timestamp: std::time::Instant,
    /// The emote's cached image data.
    image: image::DynamicImage,
}

pub struct Plugin {
    /// Details about the user's terminal.
    tty: TTY,
    /// A mapping of all the global Twitch emote IDs with their actual text codes.
    global_emotes: crate::utils::EmoteIDs,
    /// The currently rendered emotes from Twitch chat.
    active_emotes: Vec<ActiveEmote>,
    /// The current output of all emotes to be sent to Tattoy.
    output: Vec<tattoy_protocol::Pixel>,
    /// The time at which the previous frame was rendererd.
    last_frame_tick: tokio::time::Instant,
}

impl Plugin {
    /// Instatiate
    fn new() -> Result<Self> {
        Ok(Self {
            tty: TTY {
                size: (0, 0),
                cursor_position: (0, 0),
                cells: Vec::new(),
            },
            global_emotes: crate::utils::load_emotes()?,
            active_emotes: Vec::default(),
            output: Vec::default(),
            last_frame_tick: tokio::time::Instant::now(),
        })
    }

    /// Our main entrypoint.
    pub(crate) async fn start(
        mut tattoy_messages: tokio::sync::mpsc::Receiver<tattoy_protocol::PluginInputMessages>,
    ) -> Result<()> {
        let mut plugin = Self::new()?;

        let (bot_messages_tx, mut bot_messages) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            let result = Self::bot_listener(bot_messages_tx).await;
            if let Err(error) = result {
                tracing::error!("Bot listender crashed: {error:?}");
            }
        });

        #[expect(
            clippy::integer_division_remainder_used,
            reason = "This is caused by the `tokio::select!`"
        )]
        loop {
            tokio::select! {
                () = plugin.sleep_until_next_frame_tick() => {
                    plugin.render().await?;
                }
                Some(message) = bot_messages.recv() => {
                    plugin.handle_bot_message(message).await?;
                }
                Some(message) = tattoy_messages.recv() => {
                    plugin.handle_tattoy_message(message);
                }
            }
        }

        #[expect(unreachable_code, reason = "We rely on Tattoy to shut us down")]
        Ok(())
    }

    async fn bot_listener(
        messages: tokio::sync::mpsc::UnboundedSender<tattoy_twitch_tombh_plugin::BotMessage>,
    ) -> Result<()> {
        tracing::info!("Listening for connections from Tom's Twitch bot...");
        if std::path::PathBuf::new()
            .join(tattoy_twitch_tombh_plugin::SOCKET_PATH)
            .exists()
        {
            std::fs::remove_file(tattoy_twitch_tombh_plugin::SOCKET_PATH)?;
        }
        let listener = tokio::net::UnixListener::bind(tattoy_twitch_tombh_plugin::SOCKET_PATH)?;
        let mut buffer = String::new();
        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    tracing::info!("Received new connection from Tom's Twitch bot");
                    let mut reader = tokio::io::BufReader::new(stream);
                    loop {
                        if let Err(error) = reader.read_line(&mut buffer).await {
                            tracing::error!("Bot connection error: {error}");
                            break;
                        }
                        tracing::debug!("Data received from bot: {buffer}");
                        if buffer.is_empty() {
                            tracing::info!("Bot disconnected.");
                            break;
                        }
                        let message = serde_json::from_str(buffer.trim())?;
                        messages.send(message)?;
                        buffer.clear();
                    }
                }
                Err(error) => {
                    tracing::error!("Unix domain socket listener error: {error:?}");
                    break;
                }
            }
        }

        Ok(())
    }

    // {"username": "tom", "regexish": "nightly", "emote": "LUL"}
    async fn handle_bot_message(
        &mut self,
        message: tattoy_twitch_tombh_plugin::BotMessage,
    ) -> Result<()> {
        tracing::info!("Adding active emote: {message:?}");
        self.add_active_emote(message.emote, message.regexish).await
    }

    /// Sleep until the next frame render is due.
    pub async fn sleep_until_next_frame_tick(&mut self) {
        let target = crate::renderer::ONE_MICROSECOND.wrapping_div(TARGET_FRAME_RATE);
        let target_frame_rate_micro = std::time::Duration::from_micros(target);
        if let Some(wait) = target_frame_rate_micro.checked_sub(self.last_frame_tick.elapsed()) {
            tokio::time::sleep(wait).await;
        }
        self.last_frame_tick = tokio::time::Instant::now();
    }

    /// Handle a protocol message from Tattoy.
    #[expect(clippy::todo, reason = "TODO: support terminal resizing")]
    fn handle_tattoy_message(&mut self, message: tattoy_protocol::PluginInputMessages) {
        match message {
            tattoy_protocol::PluginInputMessages::PTYUpdate {
                size,
                cells,
                cursor,
            } => {
                self.tty.size = size;
                self.tty.cells = cells;
                self.tty.cursor_position = cursor;
            }
            tattoy_protocol::PluginInputMessages::TTYResize { .. } => todo!(),

            #[expect(
                clippy::unreachable,
                reason = "
                    Tattoy uses `#[non-exhaustive]` so have always be able to handle new
                    message kinds without crashing
                "
            )]
            _ => unreachable!(),
        }
    }

    // | () -- () |
    // |    H     |
    // |   \ /    |
    // |   ^^^    |
    // — @YourInty April 23rd 2025
    //
    async fn add_active_emote(&mut self, code: String, regexish: String) -> Result<()> {
        match self.global_emotes.get(&code) {
            Some(id) => {
                let image = self.get_emote_image(id).await?;
                let active_emote = ActiveEmote {
                    regexish,
                    image,
                    timestamp: std::time::Instant::now(),
                };
                tracing::debug!("Generated active emote: {active_emote:?}");
                self.active_emotes.push(active_emote);
            }
            None => {
                tracing::warn!("Couldn't find ID for emote code: {code}");
            }
        }

        Ok(())
    }

    async fn get_emote_image(&self, emote_id: &str) -> Result<image::DynamicImage> {
        let url = format!("https://static-cdn.jtvnw.net/emoticons/v2/{emote_id}/static/light/3.0");
        let resp = reqwest::get(url).await?;

        let emote_big =
            image::load_from_memory_with_format(&resp.bytes().await?, image::ImageFormat::Png)?;

        Ok(emote_big)
    }

    /// Send a frame to Tattoy.
    async fn render(&mut self) -> Result<()> {
        if self.tty.size.0 == 0 || self.tty.size.1 == 0 {
            return Ok(());
        }

        self.cleanup().await?;
        self.render_emotes().await?;
        self.send_output()?;

        Ok(())
    }

    async fn render_emotes(&mut self) -> Result<()> {
        self.output = Vec::default();
        for emote in self.active_emotes.clone() {
            self.render_emote(emote).await?;
        }

        Ok(())
    }

    async fn render_emote(&mut self, emote: ActiveEmote) -> Result<()> {
        let maybe_match = self.find_text_coordinates(emote.regexish.clone())?;

        let Some((match_x, match_y)) = maybe_match else {
            tracing::debug!("Couldn't find '{}' in TTY", emote.regexish);
            return Ok(());
        };

        let emote_resized = emote.image.resize(
            emote.regexish.len().try_into()?,
            self.tty.size.1.into(),
            image::imageops::FilterType::Lanczos3,
        );

        let half_the_emote_height = emote_resized.height() / 2;
        let emote_x = u32::try_from(match_x)?;
        let emote_y = (u32::try_from(match_y)? * 2) - half_the_emote_height;

        for pixel_y in 0..emote_resized.height() {
            for pixel_x in 0..emote_resized.width() {
                let image_pixel_u8 = emote_resized.get_pixel(pixel_x, pixel_y).0;
                let image_pixel_f32 = (
                    f32::from(image_pixel_u8[0]) / 255.0,
                    f32::from(image_pixel_u8[1]) / 255.0,
                    f32::from(image_pixel_u8[2]) / 255.0,
                    1.0,
                );
                let pixel = tattoy_protocol::Pixel::builder()
                    .coordinates((emote_x + pixel_x, emote_y + pixel_y))
                    .color(image_pixel_f32)
                    .build();
                self.output.push(pixel);
            }
        }

        Ok(())
    }

    fn find_text_coordinates(&self, regexish: String) -> Result<Option<(usize, usize)>> {
        let mut lines = Vec::<String>::new();
        for y in 0..self.tty.size.1 {
            let mut line = String::new();
            for x in 0..self.tty.size.0 {
                let maybe_cell = self
                    .tty
                    .cells
                    .iter()
                    .find(|cell| cell.coordinates == (x.into(), y.into()));
                match maybe_cell {
                    Some(cell) => line.push(cell.character),
                    None => line.push(' '),
                }
            }
            lines.push(line);
        }

        let mut maybe_x: Option<usize> = None;
        let mut y = 0;
        for (line_number, line) in lines.iter().enumerate() {
            y = line_number;
            let maybe_byte_offset = line.find(&regexish);
            if let Some(byte_offset) = maybe_byte_offset {
                maybe_x = Some(line[..byte_offset].chars().count());
                break;
            }
        }

        if let Some(x) = maybe_x {
            tracing::debug!("regexish matched to coordinates: {x}x{y}");
            return Ok(Some((x, y)));
        };

        Ok(None)
    }

    async fn cleanup(&mut self) -> Result<()> {
        let duration = std::time::Duration::from_secs(10);
        let now = std::time::Instant::now();
        self.active_emotes
            .retain(|emote| now - emote.timestamp < duration);

        Ok(())
    }

    /// Send a frame to Tattoy.
    fn send_output(&self) -> Result<()> {
        let json = serde_json::to_string(&tattoy_protocol::PluginOutputMessages::OutputPixels(
            self.output.clone(),
        ))?;
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(json.as_bytes())?;
        Ok(())
    }
}
