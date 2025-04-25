#!/bin/bash

# This just helps me to always make sure that the Tattoy parent process is always running
# the code that I'm actually developing.

PLUGIN_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$PLUGIN_DIR" || exit
cargo build --release >/dev/null 2>&1
/publicish/Workspace/tbhbot/target/release/tattoy_twitch_tombh_plugin
