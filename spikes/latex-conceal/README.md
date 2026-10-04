# latex-conceal spike (throwaway)

Question: can a stock egui 0.33 `TextEdit` show rendered math inline, reveal raw
source only under the caret, and keep caret/click/selection mapping correct?

Answer: yes. A custom layouter keeps one glyph per source char. Each concealed
span becomes invisible 0.01 pt glyphs whose `extra_letter_spacing` reserves the
equation width and whose `line_height` grows the row. Images are painted over
`math_rect`. A changed reveal set triggers `ctx.request_discard`, so there are no
stale frames.

```sh
cargo +stable test              # 11 headless tests driving a real TextEdit
cargo +stable run -- 'text $x^2$'  # demo with a fake renderer and 120 ms latency
```

Design write-up: `docs/latex-editing-design.md` in the Marker project store.
