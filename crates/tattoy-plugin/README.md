# Tom's Tattoy-Twitch plugin

Get latest gobal emotes: 
  `tbx twitch api --unformatted get /chat/emotes/global | rg -v "^done" >crates/tattoy-plugin/global_emotes.json`
