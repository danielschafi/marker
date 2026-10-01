# Marker

Fast, minimal PDF annotation, built to stay out of the way.

Marker is a desktop PDF annotator written in Rust with
[egui/eframe](https://github.com/emilk/egui) and
[MuPDF](https://mupdf.com/). Open a document, mark it up, and keep moving.

## Features

- Highlight text; add text, rectangles, ellipses, and lines.
- Typeset math annotations from LaTeX via Typst (see [Math / LaTeX](#math--latex)).
- Paste images directly from the system clipboard.
- Work across tabs with document outlines, search, and recent files.
- Navigate with the mouse, trackpad, page keys, or Vim-style motions.
- Undo and redo edits; changes auto-save back into the PDF.
- Keep the interface spare with a slim toolbar, a hideable tools strip, and
  fullscreen zen mode.
- Adjust annotation color, text size, or stroke width from an inline style bar.
- Follow the OS light/dark preference (and GNOME accent color on Linux).
- Split tabs side-by-side or stacked with a draggable divider.

## Build from source

Marker requires a stable Rust toolchain. From a checkout:

```sh
cargo run --release
```

Pass one or more PDFs to open them immediately:

```sh
cargo run --release -- document.pdf other.pdf
```

On Linux, a second launch forwards paths to the running Marker window and opens
them as tabs (single-instance). Force a separate window with `--new-window`
(or `-n`), or use the desktop action **Open in New Window**. From a tab’s
right-click menu, choose **Move to new window** to detach that document.

To build without running:

```sh
cargo build --release
./target/release/marker document.pdf
```

### Linux dependencies

The current MuPDF configuration links its codec and font libraries through
`pkg-config` and uses `bindgen`. On Debian or Ubuntu, install the build tools
and development packages with:

```sh
sudo apt install build-essential pkg-config libclang-dev \
  libfontconfig1-dev libfreetype6-dev libharfbuzz-dev libjbig2dec0-dev \
  liblcms2-dev libjpeg-dev libopenjp2-7-dev libbrotli-dev zlib1g-dev
```

Package names vary on other distributions.

## Usage

Open a PDF with `Ctrl+O`, by dropping it onto the window, or by passing its
path on the command line. Marker writes annotations directly into the opened
PDF and auto-saves after a short idle period; `Ctrl+S` saves immediately.

### Tools

| Key | Tool |
| --- | --- |
| `S` | Select text or objects (drag empty space to marquee-select by center), move, and resize |
| `H` | Highlight |
| `T` | Text |
| `R` | Rectangle |
| `E` | Ellipse |
| `I` | Line |
| `M` | Math |
| `Ctrl+Shift+V` | Paste an image from the clipboard |

### Shortcuts

| Shortcut | Action |
| --- | --- |
| `Ctrl+O` | Open a PDF |
| `Ctrl+S` | Save now |
| `Ctrl+F` or `/` | Search |
| `N` / `Shift+N` | Next / previous search result |
| `Ctrl+W` | Close the current tab |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Switch tabs |
| `Tab` / `Shift+Tab` | Cycle math templates (while editing a math annotation) |
| `Ctrl+Z` | Undo |
| `Ctrl+Y` or `Ctrl+Shift+Z` | Redo |
| `Ctrl+Shift+B` | Show or hide the tools strip |
| `F11` | Toggle zen mode |
| Click title / `Ctrl+Tab` | Open or cycle tabs |
| Drag tab (title or list) to viewport edge | Split side-by-side (left/right) or stacked (top/bottom) |
| Right-click title or tab in list | Split side-by-side / stacked / with…, unsplit, or move to a new window |
| `Ctrl+\` / `Ctrl+Shift+\` | Toggle side-by-side / stacked split with next tab |
| `Ctrl+Alt+\` | Focus the other split pane |
| `Ctrl+Alt+I` | Toggle Cursor assistant (hiding keeps the chat session) |
| `Ctrl+=` / `Ctrl+-` | Zoom in / out |
| `Ctrl+0` | Fit width, then fit height |
| `Ctrl+G` | Jump to a page |
| `Ctrl+Shift+Enter` | Insert a blank page after the current page |
| `Ctrl+C` | Copy selected page text (or highlight / text annotation) |
| `Delete` or `Backspace` | Delete the selected annotation(s) |

Vim-style navigation uses `J`/`K` to pan vertically and `L`/`Shift+L` to pan
horizontally (`H` is Highlight), plus `Ctrl+U`/`Ctrl+D` for half-page movement,
`gg` for the first page, and `G` for the last. Numeric counts work with these
motions.

### Math / LaTeX

Math annotations accept a LaTeX math subset (converted with
[MiTeX](https://github.com/mitex-rs/mitex), rendered with Typst). Wrappers like
`$...$`, `$$...$$`, `\(...\)`, and `\[...\]` are stripped automatically.

While the math editor is focused, **Tab** / **Shift+Tab** cycle starter
templates (fraction, quadratic formula, `aligned`, `bmatrix`, …). Click away or
press Escape to leave the field; **Ctrl+Tab** still switches document tabs.

**Supported (common constructs):**

- Fractions: `\frac{a}{b}`
- Roots: `\sqrt{x}`, `\sqrt[n]{x}`
- Greek letters, superscripts/subscripts, `\pm`, `\dots`, `\vdots`, `\ddots`
- Matrices: `matrix`, `pmatrix`, `bmatrix`, `Bmatrix`, `vmatrix`, `Vmatrix`
- Alignment: `aligned`, `align`, `gathered`, `gather`, `split` (and `*at` variants)
- Styling helpers: `\mathbf{...}`, `\operatorname{...}`, `\overbrace{...}`, `\underbrace{...}`

**Known gaps:** full LaTeX packages, custom macros, and MiTeX helpers without a
native Typst rewrite may fail to compile. Prefer the templates or the commands
above for reliable results.

## Configuration

Marker stores `config.toml` in the platform config directory provided by the
Rust `directories` crate:

- Linux: `${XDG_CONFIG_HOME:-$HOME/.config}/marker/config.toml`
- macOS: `~/Library/Application Support/marker/config.toml`
- Windows: `%APPDATA%\marker\config\config.toml`

The file contains annotation style defaults, tools-strip visibility, and the
recent-files list.

## License

Marker is released under the [MIT License](LICENSE).
