A stick figure that lives on your Mac desktop, fights your cursor, and keeps a diary about how you treat it. Its moods come from a local AI (Ollama); nothing goes to the cloud.

## Install

1. Download **Stickman-…-macos-arm64.zip** below (Apple Silicon Macs, macOS 13 or later) and unzip it.
2. Move **Stickman.app** to your Applications folder.
3. Open it. The app isn't signed with an Apple Developer ID, so macOS blocks the first launch:
   - Go to **System Settings → Privacy & Security**, scroll down, and click **Open Anyway** next to the Stickman message.
   - Or, in Terminal: `xattr -dr com.apple.quarantine /Applications/Stickman.app`
4. It appears in the menu bar (no Dock icon). Use the menu bar icon for Settings, and **Quit** (⌘Q) to watch it burn.

## Permissions (both optional)

- **Accessibility:** lets it shake and shove your windows.
- **Screen Recording:** lets it find the text in your active window so it can stand on lines and throw words. It captures only that window, reads it with Apple's on-device Vision, and keeps nothing.

## The AI brain (optional)

Install [Ollama](https://ollama.com) and run `ollama pull llama3.2:1b`. Without it, a built-in director takes over.

Source and full docs: https://github.com/surenjanath/stickman
