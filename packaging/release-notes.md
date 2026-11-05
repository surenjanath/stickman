A stick figure that lives on your Mac desktop, fights your cursor, and keeps a diary about how you treat it. Its moods come from a local AI (Ollama); nothing goes to the cloud.

## What's new in 0.3.0

- **Lasso:** in a fight it twirls a lasso, ropes your pointer and hauls it in hand over hand. Pull against it and it gets dragged across the floor; yank hard and the rope snaps.
- **Drawn weapons:** it draws weapons in the air in bold coloured marker, then takes the drawing off the page into its hands: a cannon, a sword, a hammer, a bow (arrows) or bombs that explode.
- **It mixes it up:** moves it just used are less likely for a while, its fighting mood changes every few seconds, and it never sticks to one plan.
- **It won't let go:** it can grab your cursor and hang off it, swinging and pounding on the arrow, until it gets bored.
- **More tools:** an eraser, a paint bucket, and a saw that cuts toolbars off windows (a copy; your window is untouched).
- **Settings:** a permissions section, a first-launch guide, a Diary tab and an About tab.

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
