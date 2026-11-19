# X and TikTok provider embeds

Baseline `275a855` (main) with only the evidence fixture added; the changed build is this
branch. Release `profile_preview --features demo` framebuffer exports at 1120×1200:

```sh
target/release/examples/profile_preview --demo --page=embeds --width=1120 --height=1200 --output=after.png
```

The fixture page adds two synthetic video cards whose post links point at `x.com` and
`www.tiktok.com`. No account, Discord transport, microphone, browser navigation or network
request is involved: the offline demo never opens a provider webview, so the captures show
the card state only.

Before, both cards fall back to "Video preview playback opens in your browser" with an
"Open video…" action, because the app had no player for these post links. After, each card
shows the same in-app play button as a YouTube card, so the post opens in the provider
player instead of the system browser. The unrelated synthetic `example.org` clip above them
keeps its browser fallback, as it is not a known provider.
