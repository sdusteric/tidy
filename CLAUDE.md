# Tidy

## Installing and updates (user preferences)

- Install new builds to ~/Applications without asking, but only when the app is not running (scripts/bundle.sh --release --install). Never quit a running copy to install unless the user asks.
- Auto-update goes through Sparkle 2.10.0 and the public sdusteric/app-updates repo; the contract is ~/Documents/Projects/mac-apps/app-updates/CONTRACT.md. Keep it wired in every release; agents run release.sh only with --dry-run.
