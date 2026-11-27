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

# Tests
test:
    cargo test
