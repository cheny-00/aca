#!/bin/sh
# aca.discord: take in what you said in the channel (the timer, once a minute) or, with check, say what the bot can see.
# Reading takes a bot (a webhook can only post): with token, channel or user empty nothing is asked of Discord.
if [ -z "$ACA_SETTING_TOKEN" ] || [ -z "$ACA_SETTING_CHANNEL" ] || [ -z "${ACA_SETTING_USER:-$ACA_SETTING_MENTION}" ]; then
  [ "$1" != check ] || echo "aca.discord: set token, channel and user (or mention) to take messages in"
  exit 0
fi
command -v python3 >/dev/null 2>&1 || { echo "aca.discord: needs python3"; exit 0; }
exec python3 "$(dirname "$0")/discord.py" "$@"
