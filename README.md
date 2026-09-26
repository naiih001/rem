# rem

A Rust-based terminal UI coding assistant for working on local projects in your repository.

## Description

`rem` is a lightweight TUI tool that helps you inspect a codebase, manage context, and interact with AI-assisted project workflows from the terminal. It is designed for local development and follows a repository-oriented workflow.

## Features

- Terminal-based user interface
- Project-aware local context
- Rust + Tokio backend
- Works with environment-based configuration

## Getting started

1. Copy the example environment file:
   ```bash
   cp .env.example .env
   ```
2. Fill in the required `REM_*` values in `.env`.
3. Run the app:
   ```bash
   cargo run
   ```

## Project structure

- `src/` - application source code
- `docs/` - documentation
- `Cargo.toml` - Rust project configuration

## License

This project is currently unlicensed unless otherwise specified.
