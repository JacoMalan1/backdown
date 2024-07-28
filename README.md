# Vertebrae

## Disclaimer

Vertebrae is an experimental piece of software. At the moment it may not be safe to rely upon it for consistent backups.
It is possible that serious bugs exist that may corrupt files/backups.

We, the project maintainers, take **NO RESPONSIBILITY** for any loss or damage resulting from the use of this program.

## Description

Vertebrae is a configurable, automatic backup utility for GNU/Linux written in Rust.

## Building

To build Vertebrae, run the following command from the root of the project: `cargo build --release`
The resulting binary will be stored in the `target/release` folder. If this folder does not exist, the binary is most likely stored
at `$CARGO_TARGET_DIR/release`
