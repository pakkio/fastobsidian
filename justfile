set shell := ["powershell.exe", "-NoLogo", "-Command"]

vault := "Z:/obsidian"

# Serve the vault at http://127.0.0.1:8765/ and open the browser
run:
    python obsidian_explorer.py "{{vault}}" --serve --open

# Write a read-only static explorer.html for the vault
static:
    python obsidian_explorer.py "{{vault}}" -o explorer.html --open
