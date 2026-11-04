# NoSteamWebHelper-Linux

A Linux implementation of [Aetopia/NoSteamWebHelper](https://github.com/Aetopia/NoSteamWebHelper).

## What it does

- Running this program launches the Steam client, forwarding the command-line arguments.
- Monitors Steam processes using the Linux Process Event Connector (netlink `NETLINK_CONNECTOR` + `CN_IDX_PROC`)
- When a game is launched, it kills all `steamwebhelper` processes.
- Includes a system tray, to control whether the program should be running.

## Usage

1.  Download the `NoSteamWebHelper` from the [Releases](https://github.com/Rothes/NoSteamWebHelper-Linux/releases) page.
2.  Place `NoSteamWebHelper` into `$HOME/.local/bin/`.
3.  Edit `steam.desktop`, Under `[Desktop Entry]` replace `/usr/bin/steam` with `NoSteamWebHelper` in the `Exec` entry.
4.  Launch Steam.
