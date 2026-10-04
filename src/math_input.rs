//! Autopair, tab-out, and command completion for math in text boxes (R7–R9).
//!
//! `apply` is a pure pre-filter in front of `TextEdit`: when it returns `Some`,
//! the caller writes the new buffer and caret and consumes the event so egui's
//! undoer sees one replacement per key. Completion accept is also pure; the
//! popup itself lives in the editor overlay.

use egui::text::CCursor;
use egui::text_selection::CCursorRange;
use egui::{Event, Key, Modifiers};

use crate::math_spans::{self, MathSpanRef};

/// One LaTeX command offered by the completion popup.
#[derive(Clone, Copy, Debug)]
pub struct LatexCommand {
    pub name: &'static str,
    /// Unicode preview shown beside the name (α, ∑, →).
    pub preview: &'static str,
    /// Inserted text. `|` marks the caret; empty `{}` are placeholder stops.
    pub snippet: &'static str,
}

/// `\partial` word under the caret inside math (`\` + at least one letter).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionQuery {
    /// Char index of the `\`.
    pub start: usize,
    /// Char index just after the typed letters (usually the caret).
    pub end: usize,
    pub prefix: String,
}

/// Apply one input event as an autopair / tab-out / autosnippet edit.
///
/// Returns `None` when the event should fall through to `TextEdit`.
/// `autosnippets` gates opt-in triggers (`//`, `mk`, `dm`).
pub fn apply(
    text: &str,
    sel: CCursorRange,
    event: &Event,
    autosnippets: bool,
) -> Option<(String, CCursorRange)> {
    match event {
        Event::Text(payload) => {
            let mut chars = payload.chars();
            let ch = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            apply_char(text, sel, ch, autosnippets)
        }
        Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => match *key {
            Key::Backspace if plain_modifiers(modifiers) => apply_backspace(text, sel),
            Key::Tab if tab_modifiers(modifiers) => apply_tab(text, sel, modifiers.shift),
            _ => None,
        },
        _ => None,
    }
}

/// Prefix match against the static table: shorter names first, then A–Z.
pub fn rank_completions(prefix: &str) -> Vec<&'static LatexCommand> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<&'static LatexCommand> = COMMANDS
        .iter()
        .filter(|cmd| cmd.name.starts_with(prefix))
        .collect();
    hits.sort_by(|a, b| {
        a.name
            .len()
            .cmp(&b.name.len())
            .then_with(|| a.name.cmp(b.name))
    });
    hits
}

/// Active `\`-command query at `caret` when the caret is inside math.
pub fn completion_query(text: &str, caret: usize) -> Option<CompletionQuery> {
    if caret == 0 {
        return None;
    }
    let byte = char_to_byte(text, caret);
    math_spans::caret_inside_math(text, byte)?;

    let chars: Vec<(usize, char)> = text.char_indices().collect();
    if caret > chars.len() {
        return None;
    }
    let mut i = caret;
    while i > 0 {
        let ch = chars[i - 1].1;
        if ch.is_ascii_alphabetic() {
            i -= 1;
            continue;
        }
        break;
    }
    if i == 0 || chars[i - 1].1 != '\\' {
        return None;
    }
    // Odd number of backslashes → real command start (not `\\cmd`).
    let mut backs = 0usize;
    let mut j = i;
    while j > 0 && chars[j - 1].1 == '\\' {
        backs += 1;
        j -= 1;
    }
    if backs % 2 == 0 {
        return None;
    }
    let start = i - 1;
    let prefix: String = chars[i..caret].iter().map(|(_, c)| *c).collect();
    if prefix.is_empty() {
        return None;
    }
    Some(CompletionQuery {
        start,
        end: caret,
        prefix,
    })
}

/// Replace the typed `\prefix` with `cmd.snippet`, caret on the first `|`.
pub fn accept_completion(
    text: &str,
    query: &CompletionQuery,
    cmd: &LatexCommand,
) -> (String, CCursorRange) {
    let start_byte = char_to_byte(text, query.start);
    let end_byte = char_to_byte(text, query.end);
    let (insert, caret_off) = expand_snippet(cmd.snippet);
    splice(text, start_byte, end_byte, &insert, caret_off)
}

fn expand_snippet(snippet: &str) -> (String, usize) {
    if let Some(pipe) = snippet.find('|') {
        let mut out = String::with_capacity(snippet.len() - 1);
        out.push_str(&snippet[..pipe]);
        out.push_str(&snippet[pipe + 1..]);
        (out, pipe)
    } else {
        (snippet.to_string(), snippet.len())
    }
}

fn plain_modifiers(modifiers: &Modifiers) -> bool {
    modifiers.is_none()
}

fn tab_modifiers(modifiers: &Modifiers) -> bool {
    modifiers.is_none()
        || (modifiers.shift
            && !modifiers.alt
            && !modifiers.ctrl
            && !modifiers.mac_cmd
            && !modifiers.command)
}

fn apply_char(
    text: &str,
    sel: CCursorRange,
    ch: char,
    autosnippets: bool,
) -> Option<(String, CCursorRange)> {
    let range = sel.as_sorted_char_range();
    if range.start != range.end {
        return (ch == '$').then(|| wrap_selection(text, range.start, range.end));
    }
    let caret = range.start;
    let byte = char_to_byte(text, caret);
    let paired = match ch {
        '$' => apply_dollar(text, caret, byte),
        '{' => apply_open_brace(text, byte),
        '(' => apply_open_paren(text, byte),
        '}' | ')' => step_over_char(text, caret, ch),
        _ => None,
    };
    if paired.is_some() {
        return paired;
    }
    if autosnippets {
        return apply_autosnippet(text, caret, byte, ch);
    }
    None
}

fn apply_autosnippet(
    text: &str,
    caret: usize,
    byte: usize,
    ch: char,
) -> Option<(String, CCursorRange)> {
    let inside = math_spans::caret_inside_math(text, byte).is_some();
    match ch {
        '/' if inside && nth_char(text, caret.wrapping_sub(1)) == Some('/') => {
            let start = char_to_byte(text, caret - 1);
            Some(splice(text, start, byte, r"\frac{}{}", 6))
        }
        'k' if !inside && nth_char(text, caret.wrapping_sub(1)) == Some('m') => {
            let start = char_to_byte(text, caret - 1);
            Some(splice(text, start, byte, "$$", 1))
        }
        'm' if !inside && nth_char(text, caret.wrapping_sub(1)) == Some('d') => {
            let start = char_to_byte(text, caret - 1);
            Some(splice(text, start, byte, "$$$$", 2))
        }
        _ => None,
    }
}

fn apply_dollar(text: &str, caret: usize, byte: usize) -> Option<(String, CCursorRange)> {
    if preceded_by_unescaped_backslash(text, byte) {
        return None;
    }
    if is_empty_inline_dollar_pair(text, caret) {
        let start = char_to_byte(text, caret - 1);
        let end = char_to_byte(text, caret + 1);
        return Some(splice(text, start, end, "$$$$", 2));
    }
    if next_char(text, caret) == Some('$') && math_spans::caret_inside_math(text, byte).is_some()
    {
        return Some((text.to_string(), cursor(caret + 1)));
    }
    if math_spans::caret_inside_math(text, byte).is_none() {
        return Some(splice(text, byte, byte, "$$", 1));
    }
    None
}

fn apply_open_brace(text: &str, byte: usize) -> Option<(String, CCursorRange)> {
    math_spans::caret_inside_math(text, byte)?;
    Some(splice(text, byte, byte, "{}", 1))
}

fn apply_open_paren(text: &str, byte: usize) -> Option<(String, CCursorRange)> {
    if math_spans::caret_inside_math(text, byte).is_none() {
        if ends_with_unescaped_backslash(text, byte) {
            return Some(splice(text, byte, byte, "(\\)", 1));
        }
        return None;
    }
    if text[..byte].ends_with("\\left") {
        return Some(splice(text, byte, byte, "(\\right)", 1));
    }
    None
}

fn step_over_char(text: &str, caret: usize, ch: char) -> Option<(String, CCursorRange)> {
    (next_char(text, caret) == Some(ch)).then(|| (text.to_string(), cursor(caret + 1)))
}

fn apply_backspace(text: &str, sel: CCursorRange) -> Option<(String, CCursorRange)> {
    if !sel.is_empty() {
        return None;
    }
    let caret = sel.primary.index;
    if caret == 0 {
        return None;
    }
    let byte = char_to_byte(text, caret);

    if caret >= 2
        && nth_char(text, caret.wrapping_sub(2)) == Some('$')
        && nth_char(text, caret - 1) == Some('$')
        && nth_char(text, caret) == Some('$')
        && nth_char(text, caret + 1) == Some('$')
    {
        let start = char_to_byte(text, caret - 2);
        let end = char_to_byte(text, caret + 2);
        return Some(splice(text, start, end, "", 0));
    }

    if is_empty_inline_dollar_pair(text, caret) {
        let start = char_to_byte(text, caret - 1);
        let end = char_to_byte(text, caret + 1);
        return Some(splice(text, start, end, "", 0));
    }

    if text[..byte].ends_with("\\left(") && text[byte..].starts_with("\\right)") {
        let start = byte - "\\left(".len();
        let end = byte + "\\right)".len();
        return Some(splice(text, start, end, "", 0));
    }

    if text[..byte].ends_with("\\(") && text[byte..].starts_with("\\)") {
        let start = byte - 2;
        let end = byte + 2;
        return Some(splice(text, start, end, "", 0));
    }

    if nth_char(text, caret - 1) == Some('{') && nth_char(text, caret) == Some('}') {
        let start = char_to_byte(text, caret - 1);
        let end = char_to_byte(text, caret + 1);
        return Some(splice(text, start, end, "", 0));
    }

    None
}

fn apply_tab(text: &str, sel: CCursorRange, shift: bool) -> Option<(String, CCursorRange)> {
    if !sel.is_empty() {
        return None;
    }
    let caret = sel.primary.index;
    let byte = char_to_byte(text, caret);
    let span = math_spans::caret_inside_math(text, byte)?;
    let placeholders = empty_placeholders(text, &span);

    if shift {
        let prev = placeholders.iter().copied().rev().find(|&p| p < caret);
        return Some(match prev {
            Some(p) => (text.to_string(), cursor(p)),
            None => (text.to_string(), sel),
        });
    }

    if let Some(p) = placeholders.iter().copied().find(|&p| p > caret) {
        return Some((text.to_string(), cursor(p)));
    }

    let (next, after) = math_spans::exit_math_span(text, &span);
    let index = byte_to_char(&next, after);
    Some((next, cursor(index)))
}

fn wrap_selection(text: &str, start: usize, end: usize) -> (String, CCursorRange) {
    let a = char_to_byte(text, start);
    let b = char_to_byte(text, end);
    let mut out = String::with_capacity(text.len() + 2);
    out.push_str(&text[..a]);
    out.push('$');
    out.push_str(&text[a..b]);
    out.push('$');
    out.push_str(&text[b..]);
    (out, cursor(end + 2))
}

fn empty_placeholders(text: &str, span: &MathSpanRef) -> Vec<usize> {
    let inner = &text[span.inner_start..span.inner_end];
    let bytes = inner.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'{' && bytes[i + 1] == b'}' {
            out.push(byte_to_char(text, span.inner_start + i + 1));
            i += 2;
        } else {
            i += 1;
        }
    }
    out
}

fn is_empty_inline_dollar_pair(text: &str, caret: usize) -> bool {
    nth_char(text, caret.wrapping_sub(1)) == Some('$')
        && nth_char(text, caret) == Some('$')
        && nth_char(text, caret.wrapping_sub(2)) != Some('$')
        && nth_char(text, caret + 1) != Some('$')
}

fn preceded_by_unescaped_backslash(text: &str, byte: usize) -> bool {
    ends_with_unescaped_backslash(text, byte)
}

fn ends_with_unescaped_backslash(text: &str, byte: usize) -> bool {
    let bytes = text.as_bytes();
    let mut n = 0usize;
    let mut i = byte;
    while i > 0 && bytes[i - 1] == b'\\' {
        n += 1;
        i -= 1;
    }
    n % 2 == 1
}

fn splice(
    text: &str,
    start_byte: usize,
    end_byte: usize,
    insert: &str,
    caret_offset_in_insert: usize,
) -> (String, CCursorRange) {
    let mut out = String::with_capacity(text.len() + insert.len());
    out.push_str(&text[..start_byte]);
    out.push_str(insert);
    out.push_str(&text[end_byte..]);
    let caret = byte_to_char(&out, start_byte + caret_offset_in_insert);
    (out, cursor(caret))
}

fn cursor(index: usize) -> CCursorRange {
    CCursorRange::one(CCursor::new(index))
}

fn next_char(text: &str, caret: usize) -> Option<char> {
    text.chars().nth(caret)
}

fn nth_char(text: &str, index: usize) -> Option<char> {
    text.chars().nth(index)
}

fn char_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(i, _)| i)
        .unwrap_or(text.len())
}

fn byte_to_char(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().count()
}

macro_rules! cmds {
    ($(($name:literal, $preview:literal, $snippet:literal)),* $(,)?) => {
        pub static COMMANDS: &[LatexCommand] = &[
            $(
                LatexCommand {
                    name: $name,
                    preview: $preview,
                    snippet: $snippet,
                },
            )*
        ];
    };
}

cmds! {
    // Greek (lower)
    ("alpha", "α", r"\alpha"),
    ("beta", "β", r"\beta"),
    ("gamma", "γ", r"\gamma"),
    ("delta", "δ", r"\delta"),
    ("epsilon", "ϵ", r"\epsilon"),
    ("varepsilon", "ε", r"\varepsilon"),
    ("zeta", "ζ", r"\zeta"),
    ("eta", "η", r"\eta"),
    ("theta", "θ", r"\theta"),
    ("vartheta", "ϑ", r"\vartheta"),
    ("iota", "ι", r"\iota"),
    ("kappa", "κ", r"\kappa"),
    ("lambda", "λ", r"\lambda"),
    ("mu", "μ", r"\mu"),
    ("nu", "ν", r"\nu"),
    ("xi", "ξ", r"\xi"),
    ("pi", "π", r"\pi"),
    ("varpi", "ϖ", r"\varpi"),
    ("rho", "ρ", r"\rho"),
    ("varrho", "ϱ", r"\varrho"),
    ("sigma", "σ", r"\sigma"),
    ("varsigma", "ς", r"\varsigma"),
    ("tau", "τ", r"\tau"),
    ("upsilon", "υ", r"\upsilon"),
    ("phi", "ϕ", r"\phi"),
    ("varphi", "φ", r"\varphi"),
    ("chi", "χ", r"\chi"),
    ("psi", "ψ", r"\psi"),
    ("omega", "ω", r"\omega"),
    // Greek (upper)
    ("Gamma", "Γ", r"\Gamma"),
    ("Delta", "Δ", r"\Delta"),
    ("Theta", "Θ", r"\Theta"),
    ("Lambda", "Λ", r"\Lambda"),
    ("Xi", "Ξ", r"\Xi"),
    ("Pi", "Π", r"\Pi"),
    ("Sigma", "Σ", r"\Sigma"),
    ("Upsilon", "Υ", r"\Upsilon"),
    ("Phi", "Φ", r"\Phi"),
    ("Psi", "Ψ", r"\Psi"),
    ("Omega", "Ω", r"\Omega"),
    // Binary / relations
    ("pm", "±", r"\pm"),
    ("mp", "∓", r"\mp"),
    ("times", "×", r"\times"),
    ("div", "÷", r"\div"),
    ("cdot", "·", r"\cdot"),
    ("ast", "∗", r"\ast"),
    ("star", "⋆", r"\star"),
    ("circ", "∘", r"\circ"),
    ("bullet", "•", r"\bullet"),
    ("cap", "∩", r"\cap"),
    ("cup", "∪", r"\cup"),
    ("sqcap", "⊓", r"\sqcap"),
    ("sqcup", "⊔", r"\sqcup"),
    ("vee", "∨", r"\vee"),
    ("wedge", "∧", r"\wedge"),
    ("oplus", "⊕", r"\oplus"),
    ("ominus", "⊖", r"\ominus"),
    ("otimes", "⊗", r"\otimes"),
    ("oslash", "⊘", r"\oslash"),
    ("odot", "⊙", r"\odot"),
    ("dagger", "†", r"\dagger"),
    ("ddagger", "‡", r"\ddagger"),
    ("wr", "≀", r"\wr"),
    ("amalg", "⨿", r"\amalg"),
    ("leq", "≤", r"\leq"),
    ("geq", "≥", r"\geq"),
    ("le", "≤", r"\le"),
    ("ge", "≥", r"\ge"),
    ("ll", "≪", r"\ll"),
    ("gg", "≫", r"\gg"),
    ("neq", "≠", r"\neq"),
    ("ne", "≠", r"\ne"),
    ("equiv", "≡", r"\equiv"),
    ("approx", "≈", r"\approx"),
    ("sim", "∼", r"\sim"),
    ("simeq", "≃", r"\simeq"),
    ("cong", "≅", r"\cong"),
    ("asymp", "≍", r"\asymp"),
    ("propto", "∝", r"\propto"),
    ("models", "⊨", r"\models"),
    ("prec", "≺", r"\prec"),
    ("succ", "≻", r"\succ"),
    ("preceq", "⪯", r"\preceq"),
    ("succeq", "⪰", r"\succeq"),
    ("subset", "⊂", r"\subset"),
    ("supset", "⊃", r"\supset"),
    ("subseteq", "⊆", r"\subseteq"),
    ("supseteq", "⊇", r"\supseteq"),
    ("sqsubset", "⊏", r"\sqsubset"),
    ("sqsupset", "⊐", r"\sqsupset"),
    ("sqsubseteq", "⊑", r"\sqsubseteq"),
    ("sqsupseteq", "⊒", r"\sqsupseteq"),
    ("in", "∈", r"\in"),
    ("ni", "∋", r"\ni"),
    ("notin", "∉", r"\notin"),
    ("vdash", "⊢", r"\vdash"),
    ("dashv", "⊣", r"\dashv"),
    ("perp", "⊥", r"\perp"),
    ("mid", "∣", r"\mid"),
    ("parallel", "∥", r"\parallel"),
    ("bowtie", "⋈", r"\bowtie"),
    ("smile", "⌣", r"\smile"),
    ("frown", "⌢", r"\frown"),
    // Arrows
    ("leftarrow", "←", r"\leftarrow"),
    ("rightarrow", "→", r"\rightarrow"),
    ("leftrightarrow", "↔", r"\leftrightarrow"),
    ("Leftarrow", "⇐", r"\Leftarrow"),
    ("Rightarrow", "⇒", r"\Rightarrow"),
    ("Leftrightarrow", "⇔", r"\Leftrightarrow"),
    ("mapsto", "↦", r"\mapsto"),
    ("hookleftarrow", "↩", r"\hookleftarrow"),
    ("hookrightarrow", "↪", r"\hookrightarrow"),
    ("leftharpoonup", "↼", r"\leftharpoonup"),
    ("rightharpoonup", "⇀", r"\rightharpoonup"),
    ("leftharpoondown", "↽", r"\leftharpoondown"),
    ("rightharpoondown", "⇁", r"\rightharpoondown"),
    ("rightleftharpoons", "⇌", r"\rightleftharpoons"),
    ("uparrow", "↑", r"\uparrow"),
    ("downarrow", "↓", r"\downarrow"),
    ("updownarrow", "↕", r"\updownarrow"),
    ("Uparrow", "⇑", r"\Uparrow"),
    ("Downarrow", "⇓", r"\Downarrow"),
    ("Updownarrow", "⇕", r"\Updownarrow"),
    ("nearrow", "↗", r"\nearrow"),
    ("searrow", "↘", r"\searrow"),
    ("swarrow", "↙", r"\swarrow"),
    ("nwarrow", "↖", r"\nwarrow"),
    ("to", "→", r"\to"),
    ("gets", "←", r"\gets"),
    ("implies", "⟹", r"\implies"),
    ("iff", "⟺", r"\iff"),
    // Dots / misc symbols
    ("ldots", "…", r"\ldots"),
    ("cdots", "⋯", r"\cdots"),
    ("vdots", "⋮", r"\vdots"),
    ("ddots", "⋱", r"\ddots"),
    ("infty", "∞", r"\infty"),
    ("partial", "∂", r"\partial"),
    ("nabla", "∇", r"\nabla"),
    ("hbar", "ℏ", r"\hbar"),
    ("ell", "ℓ", r"\ell"),
    ("Re", "ℜ", r"\Re"),
    ("Im", "ℑ", r"\Im"),
    ("aleph", "ℵ", r"\aleph"),
    ("wp", "℘", r"\wp"),
    ("emptyset", "∅", r"\emptyset"),
    ("varnothing", "∅", r"\varnothing"),
    ("exists", "∃", r"\exists"),
    ("nexists", "∄", r"\nexists"),
    ("forall", "∀", r"\forall"),
    ("neg", "¬", r"\neg"),
    ("lnot", "¬", r"\lnot"),
    ("top", "⊤", r"\top"),
    ("bot", "⊥", r"\bot"),
    ("angle", "∠", r"\angle"),
    ("triangle", "△", r"\triangle"),
    ("square", "□", r"\square"),
    ("diamond", "⋄", r"\diamond"),
    ("clubsuit", "♣", r"\clubsuit"),
    ("diamondsuit", "♢", r"\diamondsuit"),
    ("heartsuit", "♡", r"\heartsuit"),
    ("spadesuit", "♠", r"\spadesuit"),
    ("flat", "♭", r"\flat"),
    ("natural", "♮", r"\natural"),
    ("sharp", "♯", r"\sharp"),
    // Accents / wrappers
    ("hat", "̂", r"\hat{|}"),
    ("widehat", "̂", r"\widehat{|}"),
    ("check", "̌", r"\check{|}"),
    ("tilde", "̃", r"\tilde{|}"),
    ("widetilde", "̃", r"\widetilde{|}"),
    ("acute", "́", r"\acute{|}"),
    ("grave", "̀", r"\grave{|}"),
    ("dot", "̇", r"\dot{|}"),
    ("ddot", "̈", r"\ddot{|}"),
    ("breve", "̆", r"\breve{|}"),
    ("bar", "̄", r"\bar{|}"),
    ("vec", "⃗", r"\vec{|}"),
    ("overline", "‾", r"\overline{|}"),
    ("underline", "_", r"\underline{|}"),
    ("overbrace", "⏞", r"\overbrace{|}"),
    ("underbrace", "⏟", r"\underbrace{|}"),
    ("sqrt", "√", r"\sqrt{|}"),
    ("frac", "⁄", r"\frac{|}{}"),
    ("dfrac", "⁄", r"\dfrac{|}{}"),
    ("tfrac", "⁄", r"\tfrac{|}{}"),
    ("binom", "⑴", r"\binom{|}{}"),
    // Delimiters / sizing
    ("left", "(", r"\left|"),
    ("right", ")", r"\right|"),
    ("big", "(", r"\big|"),
    ("Big", "(", r"\Big|"),
    ("bigg", "(", r"\bigg|"),
    ("Bigg", "(", r"\Bigg|"),
    ("langle", "⟨", r"\langle"),
    ("rangle", "⟩", r"\rangle"),
    ("lvert", "|", r"\lvert"),
    ("rvert", "|", r"\rvert"),
    ("lVert", "‖", r"\lVert"),
    ("rVert", "‖", r"\rVert"),
    ("lfloor", "⌊", r"\lfloor"),
    ("rfloor", "⌋", r"\rfloor"),
    ("lceil", "⌈", r"\lceil"),
    ("rceil", "⌉", r"\rceil"),
    // Large operators
    ("sum", "∑", r"\sum"),
    ("prod", "∏", r"\prod"),
    ("coprod", "∐", r"\coprod"),
    ("int", "∫", r"\int"),
    ("iint", "∬", r"\iint"),
    ("iiint", "∭", r"\iiint"),
    ("oint", "∮", r"\oint"),
    ("bigcap", "⋂", r"\bigcap"),
    ("bigcup", "⋃", r"\bigcup"),
    ("bigvee", "⋁", r"\bigvee"),
    ("bigwedge", "⋀", r"\bigwedge"),
    ("bigoplus", "⨁", r"\bigoplus"),
    ("bigotimes", "⨂", r"\bigotimes"),
    ("bigodot", "⨀", r"\bigodot"),
    ("biguplus", "⊎", r"\biguplus"),
    // Functions
    ("sin", "sin", r"\sin"),
    ("cos", "cos", r"\cos"),
    ("tan", "tan", r"\tan"),
    ("cot", "cot", r"\cot"),
    ("sec", "sec", r"\sec"),
    ("csc", "csc", r"\csc"),
    ("arcsin", "arcsin", r"\arcsin"),
    ("arccos", "arccos", r"\arccos"),
    ("arctan", "arctan", r"\arctan"),
    ("sinh", "sinh", r"\sinh"),
    ("cosh", "cosh", r"\cosh"),
    ("tanh", "tanh", r"\tanh"),
    ("coth", "coth", r"\coth"),
    ("log", "log", r"\log"),
    ("ln", "ln", r"\ln"),
    ("lg", "lg", r"\lg"),
    ("exp", "exp", r"\exp"),
    ("deg", "deg", r"\deg"),
    ("det", "det", r"\det"),
    ("dim", "dim", r"\dim"),
    ("ker", "ker", r"\ker"),
    ("hom", "hom", r"\hom"),
    ("arg", "arg", r"\arg"),
    ("gcd", "gcd", r"\gcd"),
    ("lcm", "lcm", r"\lcm"),
    ("lim", "lim", r"\lim"),
    ("limsup", "lim sup", r"\limsup"),
    ("liminf", "lim inf", r"\liminf"),
    ("max", "max", r"\max"),
    ("min", "min", r"\min"),
    ("sup", "sup", r"\sup"),
    ("inf", "inf", r"\inf"),
    ("Pr", "Pr", r"\Pr"),
    // Spaces / text styles
    ("quad", "␣␣", r"\quad"),
    ("qquad", "␣␣␣␣", r"\qquad"),
    ("hspace", "␣", r"\hspace{|}"),
    ("vspace", "↕", r"\vspace{|}"),
    ("text", "t", r"\text{|}"),
    ("mathrm", "m", r"\mathrm{|}"),
    ("mathbf", "b", r"\mathbf{|}"),
    ("mathsf", "s", r"\mathsf{|}"),
    ("mathtt", "t", r"\mathtt{|}"),
    ("mathit", "i", r"\mathit{|}"),
    ("mathcal", "C", r"\mathcal{|}"),
    ("mathbb", "ℕ", r"\mathbb{|}"),
    ("mathfrak", "F", r"\mathfrak{|}"),
    ("boldsymbol", "𝒃", r"\boldsymbol{|}"),
    ("operatorname", "f", r"\operatorname{|}"),
    // Matrix-ish
    ("begin", "⟦", r"\begin{|}"),
    ("end", "⟧", r"\end{|}"),
    ("matrix", "▭", r"\begin{matrix}|\\end{matrix}"),
    ("pmatrix", "(▭)", r"\begin{pmatrix}|\\end{pmatrix}"),
    ("bmatrix", "[▭]", r"\begin{bmatrix}|\\end{bmatrix}"),
    ("vmatrix", "|▭|", r"\begin{vmatrix}|\\end{vmatrix}"),
    ("Vmatrix", "‖▭‖", r"\begin{Vmatrix}|\\end{Vmatrix}"),
    ("cases", "{", r"\begin{cases}|\\end{cases}"),
    ("align", "≡", r"\begin{align}|\\end{align}"),
    ("array", "▦", r"\begin{array}{|}\\end{array}"),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caret(index: usize) -> CCursorRange {
        CCursorRange::one(CCursor::new(index))
    }

    fn sel(start: usize, end: usize) -> CCursorRange {
        CCursorRange::two(CCursor::new(start), CCursor::new(end))
    }

    fn text_event(ch: &str) -> Event {
        Event::Text(ch.into())
    }

    fn key(key: Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn apply_text(text: &str, index: usize, ch: &str) -> (String, usize) {
        let (out, range) = apply(text, caret(index), &text_event(ch), false).unwrap();
        (out, range.primary.index)
    }

    #[test]
    fn dollar_outside_inserts_pair() {
        let (out, c) = apply_text("ab", 2, "$");
        assert_eq!(out, "ab$$");
        assert_eq!(c, 3);
    }

    #[test]
    fn second_dollar_in_empty_pair_makes_display() {
        let (out, c) = apply_text("$$", 1, "$");
        assert_eq!(out, "$$$$");
        assert_eq!(c, 2);
    }

    #[test]
    fn dollar_steps_over_closer() {
        let (out, c) = apply_text("$x$", 2, "$");
        assert_eq!(out, "$x$");
        assert_eq!(c, 3);
    }

    #[test]
    fn brace_and_paren_step_over() {
        let (out, c) = apply_text("$x{}$", 3, "}");
        assert_eq!(out, "$x{}$");
        assert_eq!(c, 4);

        let (out, c) = apply_text("$()$", 2, ")");
        assert_eq!(out, "$()$");
        assert_eq!(c, 3);
    }

    #[test]
    fn backspace_deletes_empty_pairs() {
        let (out, range) =
            apply("$$", caret(1), &key(Key::Backspace, Modifiers::NONE), false).unwrap();
        assert_eq!((out.as_str(), range.primary.index), ("", 0));

        let (out, range) =
            apply("$$$$", caret(2), &key(Key::Backspace, Modifiers::NONE), false).unwrap();
        assert_eq!((out.as_str(), range.primary.index), ("", 0));

        assert!(apply("$*$", caret(2), &key(Key::Backspace, Modifiers::NONE), false).is_none());

        let (out, range) =
            apply("$({})$", caret(3), &key(Key::Backspace, Modifiers::NONE), false).unwrap();
        assert_eq!((out.as_str(), range.primary.index), ("$()$", 2));

        let (out, range) =
            apply(r"\(\)", caret(2), &key(Key::Backspace, Modifiers::NONE), false).unwrap();
        assert_eq!((out.as_str(), range.primary.index), ("", 0));

        let (out, range) = apply(
            r"$\left(\right)$",
            caret(7),
            &key(Key::Backspace, Modifiers::NONE),
            false,
        )
        .unwrap();
        assert_eq!((out.as_str(), range.primary.index), ("$$", 1));
    }

    #[test]
    fn latex_paren_autopair_outside_math() {
        let (out, c) = apply_text(r"\", 1, "(");
        assert_eq!(out, r"\(\)");
        assert_eq!(c, 2);
    }

    #[test]
    fn brace_autopair_inside_math() {
        let (out, c) = apply_text("$x$", 2, "{");
        assert_eq!(out, "$x{}$");
        assert_eq!(c, 3);
    }

    #[test]
    fn left_paren_inserts_right() {
        // Closed `$…$` from autopair: caret sits on the closer, still inside.
        let (out, c) = apply_text(r"$a\left$", 7, "(");
        assert_eq!(out, r"$a\left(\right)$");
        assert_eq!(c, 8);
    }

    #[test]
    fn dollar_wraps_selection() {
        let (out, range) = apply("hello", sel(0, 5), &text_event("$"), false).unwrap();
        assert_eq!(out, "$hello$");
        assert_eq!(range.primary.index, 7);
    }

    #[test]
    fn escaped_dollar_stays_literal() {
        assert!(apply(r"\", caret(1), &text_event("$"), false).is_none());
        let (out, c) = apply_text(r"\\", 2, "$");
        assert_eq!(out, r"\\$$");
        assert_eq!(c, 3);
    }

    #[test]
    fn tab_jumps_placeholders_then_tabout() {
        // $\frac{}{}$ — placeholders at char 7 and 9; closer at 10.
        let text = r"$\frac{}{}$";
        let (out, range) = apply(text, caret(6), &key(Key::Tab, Modifiers::NONE), false).unwrap();
        assert_eq!(out, text);
        assert_eq!(range.primary.index, 7);

        let (out, range) = apply(text, caret(7), &key(Key::Tab, Modifiers::NONE), false).unwrap();
        assert_eq!(out, text);
        assert_eq!(range.primary.index, 9);

        let (out, range) = apply(text, caret(9), &key(Key::Tab, Modifiers::NONE), false).unwrap();
        assert_eq!(out, text);
        assert_eq!(range.primary.index, 11);

        let (out, range) = apply(text, caret(9), &key(Key::Tab, Modifiers::SHIFT), false).unwrap();
        assert_eq!(out, text);
        assert_eq!(range.primary.index, 7);
    }

    #[test]
    fn tab_outside_math_falls_through() {
        assert!(apply("plain", caret(2), &key(Key::Tab, Modifiers::NONE), false).is_none());
    }

    #[test]
    fn tab_closes_unclosed_span() {
        // Unclosed inline `$` is prose; tab-out closes `$$` / `\(` spans.
        let (out, range) =
            apply(r"$$\alpha", caret(4), &key(Key::Tab, Modifiers::NONE), false).unwrap();
        assert_eq!(out, r"$$\alpha$$");
        assert_eq!(range.primary.index, 10);

        let (out, range) =
            apply(r"\(x", caret(3), &key(Key::Tab, Modifiers::NONE), false).unwrap();
        assert_eq!(out, r"\(x\)");
        assert_eq!(range.primary.index, 5);
    }

    #[test]
    fn autopair_is_one_string_replacement() {
        let (out, range) = apply("", caret(0), &text_event("$"), false).unwrap();
        assert_eq!(out, "$$");
        assert_eq!(range.primary.index, 1);
        let (out, range) = apply(&out, range, &text_event("$"), false).unwrap();
        assert_eq!(out, "$$$$");
        assert_eq!(range.primary.index, 2);
    }

    #[test]
    fn completion_ranks_prefix_shorter_first() {
        let ranked = rank_completions("al");
        assert!(!ranked.is_empty());
        assert_eq!(ranked[0].name, "aleph");
        assert!(ranked.iter().any(|c| c.name == "alpha"));
        let names: Vec<_> = ranked.iter().map(|c| c.name).collect();
        let aleph = names.iter().position(|n| *n == "aleph").unwrap();
        let alpha = names.iter().position(|n| *n == "alpha").unwrap();
        assert!(aleph < alpha);
        assert!(ranked.windows(2).all(|w| {
            w[0].name.len() < w[1].name.len()
                || (w[0].name.len() == w[1].name.len() && w[0].name <= w[1].name)
        }));
    }

    #[test]
    fn accept_frac_inserts_placeholders() {
        // Caret on the closing `$` still counts as inside for typing aids.
        let text = r"$\frac$";
        let query = completion_query(text, 6).expect("query");
        assert_eq!(query.prefix, "frac");
        let frac = rank_completions("frac")
            .into_iter()
            .find(|c| c.name == "frac")
            .expect("frac");
        let (out, range) = accept_completion(text, &query, frac);
        assert_eq!(out, r"$\frac{}{}$");
        // First empty `{}` placeholder (same index tab-out uses).
        assert_eq!(range.primary.index, 7);
    }

    #[test]
    fn accept_frac_then_tab_lands_on_next_placeholder() {
        let text = r"$\fr$";
        let query = completion_query(text, 4).expect("query");
        assert_eq!(query.prefix, "fr");
        let frac = rank_completions("fr")
            .into_iter()
            .find(|c| c.name == "frac")
            .expect("frac");
        let (out, range) = accept_completion(text, &query, frac);
        assert_eq!(out, r"$\frac{}{}$");
        assert_eq!(range.primary.index, 7);
        let (out, range) = apply(&out, range, &key(Key::Tab, Modifiers::NONE), false).unwrap();
        assert_eq!(out, r"$\frac{}{}$");
        assert_eq!(range.primary.index, 9);
    }

    #[test]
    fn autosnippets_off_by_default() {
        assert!(apply("$/$", caret(2), &text_event("/"), false).is_none());
        assert!(apply("m", caret(1), &text_event("k"), false).is_none());
        assert!(apply("d", caret(1), &text_event("m"), false).is_none());
    }

    #[test]
    fn autosnippets_fire_when_enabled() {
        let (out, range) = apply("$/$", caret(2), &text_event("/"), true).unwrap();
        assert_eq!(out, r"$\frac{}{}$");
        assert_eq!(range.primary.index, 7);

        let (out, range) = apply("m", caret(1), &text_event("k"), true).unwrap();
        assert_eq!(out, "$$");
        assert_eq!(range.primary.index, 1);

        let (out, range) = apply("d", caret(1), &text_event("m"), true).unwrap();
        assert_eq!(out, "$$$$");
        assert_eq!(range.primary.index, 2);
    }

    #[test]
    fn completion_query_needs_letter_inside_math() {
        assert!(completion_query(r"$\$", 2).is_none());
        assert!(completion_query(r"\al", 3).is_none());
        // Caret after `al` (on the closer).
        let q = completion_query(r"$\al$", 4).expect("query");
        assert_eq!(q.prefix, "al");
        assert_eq!(q.start, 1);
    }

    #[test]
    fn command_table_is_substantial() {
        assert!(COMMANDS.len() >= 180);
    }
}
