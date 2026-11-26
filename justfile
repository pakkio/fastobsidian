set shell := ["powershell.exe", "-NoLogo", "-Command"]

vault := "Z:/obsidian"

# Open the vault in the app window
run:
    cargo run --release -- "{{vault}}"

# Build the release binary (target/release/fastobsidian.exe)
build:
    cargo build --release

# Write a read-only static explorer.html for the vault
static:
    cargo run --release -- "{{vault}}" -o explorer.html

# Rust and Python tests
test:
    cargo test
    python -m unittest discover -s tests

# Python version: serve the vault at http://127.0.0.1:8765/ and open the browser
py-run:
    python obsidian_explorer.py "{{vault}}" --serve --open
