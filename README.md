# Saola Capture

Saola Capture makes screenshots and video of the screen. Saola Capture runs on the niri compositor, in the Saola desktop environment.

## What this tool does

- Saola Capture makes an image of the full screen, one region, or one window.
- Saola Capture records video of the screen.
- The edit tool can crop an image, draw lines, add text, and hide private content.
- Saola Capture keeps a list of saved images and video files.
- Saola Capture can get the color of one point on screen.
- You can select the image format, the video format, and the audio device.

## Install

### Arch Linux package

This gets the PKGBUILD file and builds the package.

```sh
cd /tmp
curl -Lo PKGBUILD https://github.com/JorDunn/saola-capture/releases/download/saola-capture-v0.1.0/PKGBUILD
makepkg -si
```

### Build from source code

These tools are needed: Rust 1.70 or later, ffmpeg, libwayland-dev, libpipewire-0.3-dev, and clang.

```sh
cargo build --release
```

## Configure

Make a file at `~/.config/saola/capture.toml`.

Each item below has a default value. A bad value prints a message. Saola Capture then uses the default value.

`save-dir` is the save folder for images. Default: the Captures folder.

```toml
save-dir = "~/Pictures/Screenshots"
```

`image-format` is the image format, webp or png. Default: webp.

```toml
image-format = "webp"
```

`webp-quality` is a number from 1 to 100. PNG has no quality number; PNG is always lossless. Default: 90.

```toml
webp-quality = 90
```

`png-also` is true or false. True saves a PNG copy along with the main format. Default: false.

```toml
png-also = false
```

`video-preset` is the video codec: hevc, av1, or h264. Default: hevc.

```toml
video-preset = "hevc"
```

hevc and av1 save Matroska; h264 saves MP4, ready for fast web playback. hevc and h264 use hardware encoding; av1 uses hardware encoding when ready, else software.

`vaapi-device` names the render device for hardware encoding. Default: Saola Capture finds one on its own.

```toml
vaapi-device = "/dev/dri/renderD128"
```

`audio` names the audio device to use. Default: none.

none records no audio; mic records the input device; system records the output device; both records both, mixed into one track.

```toml
audio = "none"
```

`audio-mic-source` names the input device. Default: Saola Capture finds one on its own.

```toml
audio-mic-source = "alsa_input.example-mic"
```

`audio-system-source` names the output device. Default: Saola Capture finds one on its own.

```toml
audio-system-source = "alsa_output.example-speakers.monitor"
```

`audio-offset` is a time shift for audio, in seconds. A positive number delays the audio. Range: −5 to 5. Default: 0.13.

```toml
audio-offset = 0.13
```

`cursor` is true or false. True shows the pointer in images and video. Default: true.

```toml
cursor = true
```

`delay` is a wait time before capture, in seconds. Default: 0.

```toml
delay = 0
```

`toasts` is true or false. True shows notices after capture. Default: true.

```toml
toasts = true
```

`copy` is true or false. True copies images to the clipboard. Default: true.

```toml
copy = true
```

## Command line

### Make an image

Saola Capture makes an image of the full screen.

```sh
saola-capture shot --fullscreen
```

Saola Capture shows a box on screen. Drag the box, then press Enter to keep it, or Escape to cancel.

```sh
saola-capture shot --region
```

This uses an exact position and size.

```sh
saola-capture shot --region --geometry 640x480+100+100
```

This is for the active window.

```sh
saola-capture shot --window
```

Saola Capture makes an image of one window, named by its window ID.

```sh
saola-capture shot --window --window-id 42
```

Saola Capture waits, then makes an image. This example waits 3 seconds.

```sh
saola-capture shot --fullscreen --delay 3
```

This example makes an image with the daemon off.

```sh
saola-capture shot --fullscreen --no-daemon --format=webp --output=/tmp
```

### Record video

This records video.

```sh
saola-capture record start
```

This command stops an active video capture.

```sh
saola-capture record stop
```

This toggles video capture on or off.

```sh
saola-capture record toggle
```

Saola Capture shows a box on screen for the region to record.

```sh
saola-capture record start --region
```

This uses an exact position and size.

```sh
saola-capture record start --region --geometry 640x480+100+100
```

This records one window, by ID.

```sh
saola-capture record start --window --window-id 42
```

This example records with system audio.

```sh
saola-capture record start --audio system
```

This turns audio off.

```sh
saola-capture record start --audio none
```

This example records for 5 seconds, then saves nothing.

```sh
saola-capture record start --dry-run
```

### Get a color

Press the screen to select a point. Saola Capture puts the color on the clipboard.

```sh
saola-capture pick-color
```

### Show the app window

This command shows the main window.

```sh
saola-capture window
```

This command shows the edit tool for one image.

```sh
saola-capture window edit ~/Pictures/Captures/Screenshot.webp
```

### Run the daemon

This starts the background service.

```sh
saola-capture daemon
```

## Configure niri

Add Saola Capture to niri.

### Install ffmpeg

ffmpeg is necessary. Run this command:

```sh
sudo pacman -S ffmpeg
```

### Add key combinations

Edit `~/.config/niri/config.kdl`. Add these three lines to the `binds` section:

```kdl
Print hotkey-overlay-title="Screenshot (full screen)" { spawn "saola-capture" "shot" "--fullscreen"; }
Mod+Shift+S hotkey-overlay-title="Screenshot (region)" { spawn "saola-capture" "shot" "--region"; }
Mod+Shift+R hotkey-overlay-title="Record (toggle)" { spawn "saola-capture" "record" "toggle"; }
```

### Run the daemon at boot

Put this line by itself, not in binds.

```kdl
spawn-at-startup "saola-capture" "daemon"
```

## Facts to know

- The region box is for one screen only. You cannot select a region across two screens.
- Saola Capture reads Wayland and PipeWire on its own. Saola Capture does not use the desktop portal protocol.
- Notices show in a card for 5 seconds, then fade. A future project component will handle notices later.
- The edit tool has no video support. To edit a recorded video, export it to a GIF or an animated WebP from the history screen. Or, use a different program.

## How Saola Capture runs

Saola Capture runs as two programs.

- The daemon handles image capture, video capture, on-screen elements (the region box, notices), the tray menu, and D-Bus messages. The daemon uses the iced library.
- The app window shows the main screen, the edit tool, and the history screen. The app window is its own program. It starts when a command opens it.

Each command (`shot`, `record`, `pick-color`, and more) sends a D-Bus message to the daemon. A command starts the daemon if it is not active.

Saola Capture saves images to disk. Saola Capture puts them on the clipboard if `copy` is true. Saola Capture sends a message when a capture is done.

## Work on the code

This builds Saola Capture.

```sh
cargo build
```

This runs the lint tool. A warning becomes an error.

```sh
cargo clippy --all-targets -- -D warnings
```

This runs the tests.

```sh
cargo test
```

This runs the daemon.

```sh
cargo run -- daemon
```

This makes a screenshot.

```sh
cargo run -- shot --fullscreen
```

## License

This project has two licenses: MIT and Apache 2.0. You can pick MIT or Apache 2.0.
See LICENSE-MIT and LICENSE-APACHE for the full text of each license.

This project uses niri-ipc. niri-ipc has the GPLv3 license.
The built program combines every dependency into one binary. That binary follows the GPLv3 terms too.
