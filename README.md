# Ambiway

Ambilight system that captures screen edges via virtual cameras and controls RGB lighting using OpenRGB. Transform your workspace with dynamic ambient lighting that matches your screen content.

## Features

- Real-time ambilight effect synchronized with your screen
- Multi-monitor support with independent configuration
- Configurable LED counts and screen indents per monitor
- Smooth color transitions to reduce flickering
- Adjustable brightness and sampling region size
- Theoretically works everywhere, but was only tested on Hyprland

## Installation

### NixOS (Recommended)
This project is packaged as a Nix flake.
Make sure you have flakes enabled in Nix.

You can run **ambiway** directly with:
```bash
nix run github:timasoft/ambiway
```

If you want to have ambiway always available in your $PATH:
```bash
nix profile install github:timasoft/ambiway
```

If you manage your NixOS configuration with flakes, add ambiway as an input in your flake.nix:
```nix
{
  inputs.ambiway.url = "github:timasoft/ambiway";

  outputs = { self, nixpkgs, ambiway, ... }:
    {
      nixosConfigurations.my-hostname = nixpkgs.lib.nixosSystem {
        system = "x86_64-linux";
        modules = [
          ./configuration.nix
          {
            environment.systemPackages = [
              ambiway.packages.x86_64-linux.default
            ];
          }
        ];
      };
    };
}
```

### Arch Linux

1. Install dependencies:
   ```bash
   sudo pacman -S opencv libx11 libxrandr pkgconf clang ffmpeg v4l-utils openrgb
   ```

2. Clone the repository:
   ```bash
   git clone https://github.com/timasoft/ambiway.git
   cd ambiway
   ```

3. Build and run with Cargo:
   ```bash
   cargo run --release
   ```

Make sure the OpenRGB server is running before starting Ambiway.

## Virtual Camera Setup

Ambiway requires video input devices to capture screen content. For ambilight functionality, you'll need to set up virtual cameras that mirror your screen.
 > Remember to replace 2, 3 to the device IDs you want to use.

### Arch Linux

1. Install v4l2loopback:
   ```bash
   sudo pacman -S v4l2loopback-dkms
   ```

2. Load the v4l2loopback module:
   ```bash
   sudo modprobe v4l2loopback devices=2 video_nr=2,3 exclusive_caps=1
   ```

3. Make persistent across reboots:
   ```bash
   echo "options v4l2loopback devices=2 video_nr=2,3 exclusive_caps=1" | sudo tee /etc/modprobe.d/v4l2loopback.conf
   echo "v4l2loopback" | sudo tee /etc/modules-load.d/v4l2loopback.conf
   ```

4. Verify your devices:
   ```bash
   v4l2-ctl --list-devices
   ```

### NixOS Configuration

Add this to your NixOS configuration:

```nix
{ config, pkgs, ... }:

{
  boot.kernelModules = [ "v4l2loopback" ];

  boot.extraModulePackages = with config.boot.kernelPackages; [
    v4l2loopback
  ];

  boot.extraModprobeConfig = ''
    options v4l2loopback devices=2 video_nr=2,3 exclusive_caps=1
  '';

  environment.systemPackages = with pkgs; [
    v4l-utils
  ];
}
```

* Verify your devices:
   ```bash
   v4l2-ctl --list-devices
   ```

This creates two virtual video devices at `/dev/video2` and `/dev/video3` as required by the default configuration. (for 2 monitors)

## Screen Capture Setup

### For X11 Users (not tested)

Use `ffmpeg` to capture your screen directly to the virtual cameras:

1. Determine your screen resolution:
   ```bash
   xrandr | grep " connected"
   ```

2. Start screen capture for each monitor:
   ```bash
   # For first monitor (replace 1920x1080 with your resolution)
   ffmpeg -f x11grab -video_size 1920x1080 -i :0.0 -f v4l2 /dev/video2 &

   # For second monitor (adjust coordinates as needed)
   ffmpeg -f x11grab -video_size 1920x1080 -i :0.0+1920,0 -f v4l2 /dev/video3 &
   ```

3. Add to your startup applications (e.g., `.xinitrc` or desktop environment autostart):
   ```bash
   # Example for .xinitrc
   ffmpeg -f x11grab -video_size $(xrandr | grep " connected" | head -1 | awk '{print $3}' | cut -d '+' -f1) -i :0.0 -f v4l2 /dev/video2 &
   ffmpeg -f x11grab -video_size $(xrandr | grep " connected" | tail -1 | awk '{print $3}' | cut -d '+' -f1) -i :0.0+$(xrandr | grep " connected" | head -1 | awk '{print $3}' | cut -d '+' -f2),0 -f v4l2 /dev/video3 &
   ```

### For Wayland Users (Hyprland)

Hyprland users can use `wf-recorder` for lightweight screen capture:

1. Install wf-recorder:
   Arch Linux
   ```bash
   sudo pacman -S wf-recorder
   ```

   NixOS
   Install wf-recorder with your favorite way

2. Add to your Hyprland configuration (`~/.config/hypr/hyprland.conf`):
   ```ini
   # Capture monitor outputs to virtual cameras
   exec-once = wf-recorder -y -t --muxer=v4l2 --codec=rawvideo --file=/dev/video2 -o DVI-D-1
   exec-once = wf-recorder -y -t --muxer=v4l2 --codec=rawvideo --file=/dev/video3 -o HDMI-A-1
   # Start Ambiway after screen capture is set up
   exec-once = nix run github:timasoft/ambiway
   ```

   > **Note**: Replace `DVI-D-1` and `HDMI-A-1` with your actual monitor names (check with `hyprctl monitors`)

## Configuration

Create `~/.config/ambiway/config.toml` with the following structure:

```toml
[led]
left = [36, 41]   # Number of LEDs on left side for each monitor
up = [62, 76]     # Number of LEDs on top side
right = [36, 42]  # Number of LEDs on right side
down = [62, 81]   # Number of LEDs on bottom side

[indent]
# Indentation for each side of each side of each monitor
left_up = [0, 40]   # Number of pixels to indent on upper side of left side
left_down = [0, 0]  # Number of pixels to indent on lower side of left side
up_left = [0, 0]    # Number of pixels to indent on left side of upper side
up_right = [0, 0]   # Number of pixels to indent on right side of upper side
right_up = [0, 40]  # Number of pixels to indent on upper side of right side
right_down = [0, 0] # Number of pixels to indent on lower side of right side
down_left = [0, 0]  # Number of pixels to indent on left side of lower side
down_right = [0, 0] # Number of pixels to indent on right side of lower side

[settings]
size = 50              # Region size to sample (pixels)
brightness = 0.25      # Brightness multiplier (any f32)
smooth = false         # Enable color smoothing between frames
cams = [2, 3]          # Camera device IDs (/dev/video*)
device_id = 0          # OpenRGB device ID to control
zone_id_list = [1, 2]  # OpenRGB zone IDs corresponding to each monitor
```

## Usage

1. Start the OpenRGB server
2. Start your screen capture (ffmpeg or wf-recorder)
3. Verify camera devices with `v4l2-ctl --list-devices`
4. Run Ambiway:
   ```bash
   # Arch Linux
   ./target/release/ambiway

   # NixOS
   nix run github:timasoft/ambiway
   # Or with custom config
   nix run github:timasoft/ambiway -- --config /path/to/config.toml
   ```

## How It Works

1. Captures video from specified cameras (one per monitor)
2. Divides screen edges into regions based on your configuration
3. Calculates average color for each region using OpenCV
4. Sends processed color data to OpenRGB-controlled devices
5. Updates lighting in real-time (approximately 10 FPS (limited by OpenRGB))

## Dependencies

- OpenCV (with videoio support)
- X11 libraries (libX11, libXrandr)
- OpenRGB server
- Compatible RGB hardware
- Camera(s) with proper V4L2 drivers
- For X11: ffmpeg
- For Wayland: wf-recorder



<!-- ### Example config (```.config/ambiway/config.toml```) -->
<!-- ```toml -->
<!-- [led] -->
<!-- left = [36, 41] -->
<!-- up = [62, 76] -->
<!-- right = [36, 42] -->
<!-- down = [62, 81] -->
<!---->
<!-- [indent] -->
<!-- left_up = [0, 40] -->
<!-- left_down = [0, 0] -->
<!-- up_left = [0, 0] -->
<!-- up_right = [0, 0] -->
<!-- right_up = [0, 40] -->
<!-- right_down = [0, 0] -->
<!-- down_left = [0, 0] -->
<!-- down_right = [0, 0] -->
<!---->
<!-- [settings] -->
<!-- size = 50 -->
<!-- brightness = 0.25 -->
<!-- smooth = false -->
<!-- cams = [2, 3] -->
<!-- device_id = 0 -->
<!-- zone_id_list = [1, 2] -->
<!-- ``` -->
