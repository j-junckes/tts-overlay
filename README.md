# tts-overlay
Allows text-to-speech with a handy Wayland overlay. Built with GTK4.

## Known issues
It's stupid, I'm pretty sure the GTK4 code is incorrect in *some* way, but I'm not sure how exactly.

Right now it only works with `espeak`.

## TO-DOs
 - [ ] Assess and fix the current workaround for CTRL+C on the daemon that causes a spinlock
 - [ ] Allow usage of other providers:
   - [x] `espeak`
   - [ ] [ElevenLabs](https://elevenlabs.io/)
   - [ ] [kokoro](https://github.com/hexgrad/kokoro)
 - [ ] Add keybinds for insertion of emotion tags on supported providers
 - [ ] Add custom theming
 - [ ] Check if another instance of the program is running upon startup, and if so, destroy the old one and create a new one

## Usage
Run the program in daemon mode somewhere:
```shell
tts-overlay -d
```

Bind the normal execution of the program somewhere, such as on [Hyprland](https://hypr.land/) for example:
```hyprlandconf
exec-once = tts-overlay -d

# ...

bind = $mainMod SHIFT, T, exec, tts-overlay
```

## Configuration
The configuration file should live in your `$XDG_CONFIG_DIR/tts-overlay/config.toml`.

### Example
```toml
# Used for replacing text before it's sent to the TTS provider
[replacements]
"pls" = "please"
"idk" = "I don't know"
```