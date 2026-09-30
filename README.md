# rem

A Rust-based terminal UI coding assistant for working on local projects in your repository.

## Description

`rem` is a lightweight TUI tool that helps you inspect a codebase, manage context, and interact with AI-assisted project workflows from the terminal. It is designed for local development and follows a repository-oriented workflow.

## Features

- Terminal-based user interface
- Project-aware local context
- Rust + Tokio backend
- TOML-based configuration (`~/.config/rem/config.toml`)

## Getting started

1. Create the config directory:
   ```bash
   mkdir -p ~/.config/rem
   ```
2. Copy the example config:
   ```bash
   cp config.example.toml ~/.config/rem/config.toml
   ```
3. Fill in your API key and settings.
4. Run the app:
   ```bash
   cargo run
   ```

## Project structure

- `src/` - application source code
- `docs/` - documentation
- `Cargo.toml` - Rust project configuration

## License

This project is currently unlicensed unless otherwise specified.
