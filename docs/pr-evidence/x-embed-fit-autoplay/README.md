# X embed fit and one-click provider playback

Same synthetic `profile_preview --features demo` fixture at 1120×1200 with
`--scroll=-380`:

```sh
target/release/examples/profile_preview --demo --page=embeds --width=1120 --height=1200 \
  --scroll=-380 --output=after.png
```

- `before.png` is `main` at `0ca67f2`: the X stage reserves 410 px of post chrome.
- `after.png` is this change: the X stage reserves 300 px, so a typical short post does
  not leave a large empty strip below the clip.

The offline demo never opens a provider webview, so the captures show the reserved card
only. The live fixes were checked separately in a browser engine:

- The X embed page for the reported public post measured 528 px of content, while the old
  stage reserved about 719 px at 550 px wide. With the same page, one programmatic click on
  X's own "Play Video" control started the clip and it reached the end (`1:16 / 1:16`).
- The TikTok embed page reached `00:10/00:10` after one programmatic click on its own
  "Play video" control.
- The provider webview is transparent around its card; the stage now stays solid black
  during playback instead of painting the poster underneath, which was leaking the
  thumbnail through the rounded corners and the empty bottom strip.

No account, Discord transport, microphone or Serein credential is involved in the fixture.
Live Discord playback is still unverified.
