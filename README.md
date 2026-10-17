<div align="center">
  <img src="images/logo.png" alt="trUSDX Linux Audio Driver" width="200"/>
</div>

# trUSDX Linux Audio Driver

A native Linux application for interfacing with the trUSDX QRP transceiver, providing audio bridge functionality, CAT control, and rigctl protocol support.

## Features

- **Audio Bridge**: Bidirectional audio streaming between PulseAudio and the trUSDX radio
- **Dual Interface**: Both command-line (CLI) and graphical (GUI) interfaces
- **rigctl Support**: Compatible with hamlib and other rig control software via rigctl protocol
- **CAT Control**: Full CAT command support for frequency, mode, and transmit control
- **Real-time Monitoring**: Audio level meters and status display
- **AppImage Distribution**: Pre-built AppImage for easy installation on any Linux distribution

## Screenshots

### Command Line Interface

![CLI Interface](images/cli.png)

The command-line interface provides real-time monitoring of audio levels, frequency, mode, and transmit state.

### Graphical User Interface

![GUI Interface](images/gui.png)

The Slint-based graphical interface displays audio level meters and radio status in a user-friendly window.

### Audio Control

![Audio Control](images/AudioControl.png)

Set Input to TRUSDX.monitor and Output to TRUSDX

### Radio Control

![Radio Control](images/RadioControl.png)

Full control over radio parameters including frequency, mode, and transmit state.

### fldigi Integration

![fldigi Integration](images/fldigi.png)

Seamless integration with fldigi and other hamlib-compatible software via rigctl protocol.

## Requirements

- Linux system with PulseAudio
- trUSDX radio connected via USB serial port
- A display server for the Slint GUI (X11 or Wayland), or use `--no-gui`
- Appropriate permissions to access serial port (usually requires adding user to `dialout` group)

## Installation

### From AppImage

1. Download `trusdx-x86_64.AppImage` from the [Releases](https://github.com/ilikehamradio/trusdx_Linux_Audio_Cat_Driver/releases) page
2. Make it executable: `chmod +x trusdx-x86_64.AppImage`
3. Run it directly, or manage it with [Gear Lever](https://github.com/mijorus/gearlever) (updates via embedded `gh-releases-zsync`)

CI builds AppImages automatically:

- **Every push to `master`** → rolling [continuous](https://github.com/ilikehamradio/trusdx_Linux_Audio_Cat_Driver/releases/tag/continuous) release (Gear Lever channel `continuous`)
- **Version tags (`v*`)** → stable release (channel `latest`)

Each release publishes both the `.AppImage` and matching `.zsync` for delta updates.

### From Source

1. Install Rust toolchain: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
2. Install system dependencies:
   - Debian/Ubuntu: `sudo apt-get install libpulse-dev libudev-dev pkg-config libfontconfig1-dev`
   - Arch: `sudo pacman -S pulseaudio libudev fontconfig`
   - Fedora: `sudo dnf install pulseaudio-devel systemd-devel fontconfig-devel`
3. Clone the repository: `git clone <repository-url>`
4. Build: `cargo build --release`
5. Run: `./target/release/trusdx_linux_driver`

## Usage

### Command Line Interface

```bash
./target/release/trusdx_linux_driver --no-gui
./target/release/trusdx_linux_driver --device /dev/ttyUSB0
./target/release/trusdx_linux_driver --help
```

The CLI shows real-time audio levels (RX/TX), frequency, mode, transmit state, and RTS. Press `ESC` or `Ctrl-C` to shut down.

### Graphical Interface

By default the app starts in the **system tray** (no CLI level meters in that mode). Use the tray icon (click or “Show window”) to open the Slint meters UI; closing the window hides it back to the tray. Choose **Quit** from the tray menu (or press `ESC` / `Ctrl-C` in the terminal) for a full shutdown. Use `--no-gui` for terminal meters only.

On Linux you need a StatusNotifier/AppIndicator host (KDE/XFCE usually work; GNOME often needs an extension).

### rigctl Integration

The application exposes a rigctl-compatible server on `127.0.0.1:4532`. You can use it with:
- WSJT-X
- fldigi
- Any hamlib-compatible software

Example:
```bash
rigctl -m 2 -r 127.0.0.1:4532 f
```

## Building AppImage

To build an AppImage:

```bash
cargo install cargo-appimage
cargo appimage
```

The AppImage will be generated in `target/appimage/`.

## Technical Details

- **Audio Format**: RX ~7812 Hz (f32 Pulse playback), TX 11520 Hz (s16 record → 8-bit CAT stream)
- **Pulse null-sink**: Created at the TX rate (`rate=11520`) as `TRUSDX` / `TRUSDX.monitor`
- **Serial Protocol**: CAT commands over USB serial (CH340 1a86:7523, or `--device`)
- **rigctl Port**: 127.0.0.1:4532
- **Threading**: Audio bridge drains outbound serial control; a status poller sends `;FA;`/`;MD;` (leading `;` interrupts US audio) at launch and every ~2s in RX; Slint GUI runs on the main thread when enabled

## Troubleshooting

### GUI doesn't appear when launched from file manager

The application requires proper environment variables. If launching from a file manager, ensure:
- `DISPLAY` or `WAYLAND_DISPLAY` is set
- `XDG_RUNTIME_DIR` is set (for Wayland)
- `DBUS_SESSION_BUS_ADDRESS` is set

### Serial port permission denied

Add your user to the `dialout` group:
```bash
sudo usermod -a -G dialout $USER
```
Then log out and log back in.

### Audio not working

Ensure PulseAudio is running:
```bash
pulseaudio --check || pulseaudio --start
```

## Credits

1. **[FT8CN](https://github.com/N0BOY/FT8CN)** - An Android FT8 application that provided inspiration for digital mode integration

2. **[trusdx-audio](https://github.com/olgierd/trusdx-audio)** - Python implementation for TRUSDX audio over CAT

3. **[TRUSDX Audio Documentation](https://dl2man.de/wp-content/uploads/2022/01/wp.php/trusdx-audio.zip)** - Original documentation and reference implementation

## License

This project is licensed under the MIT License.

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## Acknowledgments

Special thanks to the trUSDX community and the developers of the projects listed in the Credits section for their pioneering work in trUSDX audio integration.

