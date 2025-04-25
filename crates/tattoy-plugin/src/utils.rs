// "format": [
//     "static"
//   ],
//   "id": "emotesv2_bd02ce011d11477cba6b2753e19fbd55",
//   "images": {
//     "url_1x": "https://static-cdn.jtvnw.net/emoticons/v2/emotesv2_bd02ce011d11477cba6b2753e19fbd55/static/light/1.0",
//     "url_2x": "https://static-cdn.jtvnw.net/emoticons/v2/emotesv2_bd02ce011d11477cba6b2753e19fbd55/static/light/2.0",
//     "url_4x": "https://static-cdn.jtvnw.net/emoticons/v2/emotesv2_bd02ce011d11477cba6b2753e19fbd55/static/light/3.0"
//   },
//   "name": "ZLANsup",
//   "scale": [
//     "1.0",
//     "2.0",
//     "3.0"
//   ],
//   "theme_mode": [
//     "light",
//     "dark"
//   ]
// }

const GLOBAL_EMOTES_JSON: &str = include_str!("../global_emotes.json");

use std::collections::HashMap;
pub type EmoteIDs = std::collections::HashMap<String, String>;

#[allow(dead_code)]
#[derive(serde::Deserialize)]
struct TwitchEmote {
    format: Vec<String>,
    id: String,
    images: HashMap<String, String>,
    name: String,
    scale: Vec<String>,
    theme_mode: Vec<String>,
}

#[derive(serde::Deserialize)]
struct EmotesJSON {
    data: Vec<TwitchEmote>,
}

pub fn load_emotes() -> color_eyre::eyre::Result<EmoteIDs> {
    let json: EmotesJSON = serde_json::from_str(GLOBAL_EMOTES_JSON)?;

    let mut emotes = std::collections::HashMap::new();
    for item in json.data {
        emotes.insert(item.name, item.id);
    }
    Ok(emotes)
}
