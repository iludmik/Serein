# Larger X (Twitter) embed stage

The offline `profile_preview --features demo` fixture of the same synthetic X card, at
1120×1200 with `--scroll=-380`:

```sh
target/release/examples/profile_preview --demo --page=embeds --width=1120 --height=1200 \
  --scroll=-380 --output=after.png
```

- `before.png` is the pre-fix build (`7e953d9`): the stage is sized only to the clip
  (the old 350 px media bound), so X's header, text and footer sit outside the webview
  and playback needs an inner scroll.
- `after.png` is this change: the X stage reserves the post chrome around the clip, so the
  whole post fits in the embed box.

No account, Discord transport, microphone, browser navigation or network request is
involved: the offline demo never opens a provider webview, so the captures show the
reserved card/stage only. The live page was measured separately in a browser at the
embed's 550 px width: a 9:16 clip renders in a 517 px square, the article is 895 px tall,
and the header/text/footer add roughly 378 px.
