//! TeX as plain Unicode text (`h_i` → `hᵢ`, `\alpha \le \frac{a}{b}` → `α ≤ a/b`): how a
//! formula reads where markdown can only hold text (see [`super::inline`]).

/// Symbols and words for TeX control words.
const COMMANDS: &[(&str, &str)] = &[
    ("alpha", "α"),
    ("beta", "β"),
    ("gamma", "γ"),
    ("delta", "δ"),
    ("epsilon", "ε"),
    ("varepsilon", "ε"),
    ("zeta", "ζ"),
    ("eta", "η"),
    ("theta", "θ"),
    ("vartheta", "ϑ"),
    ("iota", "ι"),
    ("kappa", "κ"),
    ("lambda", "λ"),
    ("mu", "μ"),
    ("nu", "ν"),
    ("xi", "ξ"),
    ("pi", "π"),
    ("varpi", "ϖ"),
    ("rho", "ρ"),
    ("varrho", "ϱ"),
    ("sigma", "σ"),
    ("varsigma", "ς"),
    ("tau", "τ"),
    ("upsilon", "υ"),
    ("phi", "φ"),
    ("varphi", "φ"),
    ("chi", "χ"),
    ("psi", "ψ"),
    ("omega", "ω"),
    ("Gamma", "Γ"),
    ("Delta", "Δ"),
    ("Theta", "Θ"),
    ("Lambda", "Λ"),
    ("Xi", "Ξ"),
    ("Pi", "Π"),
    ("Sigma", "Σ"),
    ("Upsilon", "Υ"),
    ("Phi", "Φ"),
    ("Psi", "Ψ"),
    ("Omega", "Ω"),
    ("cdot", "·"),
    ("times", "×"),
    ("div", "÷"),
    ("pm", "±"),
    ("mp", "∓"),
    ("ast", "∗"),
    ("circ", "∘"),
    ("bullet", "•"),
    ("oplus", "⊕"),
    ("otimes", "⊗"),
    ("le", "≤"),
    ("leq", "≤"),
    ("ge", "≥"),
    ("geq", "≥"),
    ("ne", "≠"),
    ("neq", "≠"),
    ("ll", "≪"),
    ("gg", "≫"),
    ("approx", "≈"),
    ("equiv", "≡"),
    ("sim", "∼"),
    ("simeq", "≃"),
    ("cong", "≅"),
    ("propto", "∝"),
    ("infty", "∞"),
    ("partial", "∂"),
    ("nabla", "∇"),
    ("sum", "∑"),
    ("prod", "∏"),
    ("int", "∫"),
    ("oint", "∮"),
    ("to", "→"),
    ("rightarrow", "→"),
    ("leftarrow", "←"),
    ("gets", "←"),
    ("leftrightarrow", "↔"),
    ("Rightarrow", "⇒"),
    ("implies", "⇒"),
    ("Leftarrow", "⇐"),
    ("Leftrightarrow", "⇔"),
    ("iff", "⇔"),
    ("mapsto", "↦"),
    ("uparrow", "↑"),
    ("downarrow", "↓"),
    ("in", "∈"),
    ("notin", "∉"),
    ("ni", "∋"),
    ("subset", "⊂"),
    ("subseteq", "⊆"),
    ("supset", "⊃"),
    ("supseteq", "⊇"),
    ("cup", "∪"),
    ("cap", "∩"),
    ("setminus", "∖"),
    ("emptyset", "∅"),
    ("varnothing", "∅"),
    ("forall", "∀"),
    ("exists", "∃"),
    ("neg", "¬"),
    ("lnot", "¬"),
    ("land", "∧"),
    ("wedge", "∧"),
    ("lor", "∨"),
    ("vee", "∨"),
    ("ldots", "…"),
    ("dots", "…"),
    ("cdots", "⋯"),
    ("vdots", "⋮"),
    ("ddots", "⋱"),
    ("langle", "⟨"),
    ("rangle", "⟩"),
    ("lfloor", "⌊"),
    ("rfloor", "⌋"),
    ("lceil", "⌈"),
    ("rceil", "⌉"),
    ("mid", "∣"),
    ("parallel", "∥"),
    ("perp", "⊥"),
    ("angle", "∠"),
    ("degree", "°"),
    ("prime", "′"),
    ("hbar", "ℏ"),
    ("ell", "ℓ"),
    ("aleph", "ℵ"),
    ("Re", "ℜ"),
    ("Im", "ℑ"),
    ("star", "⋆"),
    ("quad", "  "),
    ("qquad", "    "),
    ("lbrace", "{"),
    ("rbrace", "}"),
    ("vert", "|"),
    ("Vert", "‖"),
    ("backslash", "\\"),
    ("dagger", "†"),
    ("checkmark", "✓"),
];

/// Control words that set their name as an upright word (`\log x` → `log x`).
const WORDS: &[&str] = &[
    "log", "ln", "lg", "exp", "sin", "cos", "tan", "cot", "sec", "csc", "arcsin", "arccos",
    "arctan", "sinh", "cosh", "tanh", "max", "min", "sup", "inf", "lim", "liminf", "limsup", "det",
    "dim", "ker", "deg", "gcd", "arg", "Pr", "mod", "bmod",
];

/// Control words that draw their argument as it is (fonts, accents, sizing).
const TRANSPARENT: &[&str] = &[
    "text",
    "textrm",
    "textbf",
    "textit",
    "texttt",
    "mathrm",
    "mathbf",
    "mathit",
    "mathsf",
    "mathtt",
    "mathcal",
    "mathscr",
    "mathfrak",
    "boldsymbol",
    "bm",
    "operatorname",
    "hat",
    "widehat",
    "bar",
    "overline",
    "underline",
    "tilde",
    "widetilde",
    "vec",
    "dot",
    "ddot",
    "acute",
    "grave",
    "check",
    "breve",
    "mbox",
    "boxed",
];

/// Control words that draw nothing.
const SILENT: &[&str] = &[
    "left",
    "right",
    "big",
    "Big",
    "bigg",
    "Bigg",
    "bigl",
    "bigr",
    "Bigl",
    "Bigr",
    "biggl",
    "biggr",
    "limits",
    "nolimits",
    "displaystyle",
    "textstyle",
    "scriptstyle",
];

const SUPERSCRIPTS: &[(char, char)] = &[
    ('0', '⁰'),
    ('1', '¹'),
    ('2', '²'),
    ('3', '³'),
    ('4', '⁴'),
    ('5', '⁵'),
    ('6', '⁶'),
    ('7', '⁷'),
    ('8', '⁸'),
    ('9', '⁹'),
    ('+', '⁺'),
    ('-', '⁻'),
    ('=', '⁼'),
    ('(', '⁽'),
    (')', '⁾'),
    ('n', 'ⁿ'),
    ('i', 'ⁱ'),
    ('a', 'ᵃ'),
    ('b', 'ᵇ'),
    ('c', 'ᶜ'),
    ('d', 'ᵈ'),
    ('e', 'ᵉ'),
    ('f', 'ᶠ'),
    ('g', 'ᵍ'),
    ('h', 'ʰ'),
    ('j', 'ʲ'),
    ('k', 'ᵏ'),
    ('l', 'ˡ'),
    ('m', 'ᵐ'),
    ('o', 'ᵒ'),
    ('p', 'ᵖ'),
    ('r', 'ʳ'),
    ('s', 'ˢ'),
    ('t', 'ᵗ'),
    ('u', 'ᵘ'),
    ('v', 'ᵛ'),
    ('w', 'ʷ'),
    ('x', 'ˣ'),
    ('y', 'ʸ'),
    ('z', 'ᶻ'),
    ('T', 'ᵀ'),
    ('′', '′'),
    ('*', '*'),
    ('∗', '*'),
];

const SUBSCRIPTS: &[(char, char)] = &[
    ('0', '₀'),
    ('1', '₁'),
    ('2', '₂'),
    ('3', '₃'),
    ('4', '₄'),
    ('5', '₅'),
    ('6', '₆'),
    ('7', '₇'),
    ('8', '₈'),
    ('9', '₉'),
    ('+', '₊'),
    ('-', '₋'),
    ('=', '₌'),
    ('(', '₍'),
    (')', '₎'),
    ('a', 'ₐ'),
    ('e', 'ₑ'),
    ('h', 'ₕ'),
    ('i', 'ᵢ'),
    ('j', 'ⱼ'),
    ('k', 'ₖ'),
    ('l', 'ₗ'),
    ('m', 'ₘ'),
    ('n', 'ₙ'),
    ('o', 'ₒ'),
    ('p', 'ₚ'),
    ('r', 'ᵣ'),
    ('s', 'ₛ'),
    ('t', 'ₜ'),
    ('u', 'ᵤ'),
    ('v', 'ᵥ'),
    ('x', 'ₓ'),
];

const BLACKBOARD: &[(char, char)] = &[
    ('R', 'ℝ'),
    ('N', 'ℕ'),
    ('Z', 'ℤ'),
    ('Q', 'ℚ'),
    ('C', 'ℂ'),
    ('P', 'ℙ'),
    ('H', 'ℍ'),
];

/// `tex` as Unicode text.
pub fn approximate(tex: &str) -> String {
    let mut parser = Parser {
        chars: tex.chars().collect(),
        at: 0,
    };
    let out = parser.sequence(false);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Appends `text` to `out` with markdown's punctuation escaped, so it shows as written.
pub fn escaped(text: &str, out: &mut String) {
    for c in text.chars() {
        if matches!(
            c,
            '\\' | '*' | '_' | '`' | '[' | ']' | '<' | '>' | '|' | '~' | '#' | '$'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
}

struct Parser {
    chars: Vec<char>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    /// Atoms up to the end, or up to the `}` closing a group when `group`.
    fn sequence(&mut self, group: bool) -> String {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            match c {
                '}' if group => {
                    self.at += 1;
                    return out;
                }
                '^' | '_' => {
                    self.at += 1;
                    let script = self.argument();
                    out.push_str(&scripted(&script, c == '^'));
                }
                _ => out.push_str(&self.atom()),
            }
        }
        out
    }

    /// One atom: a group, a control sequence or a character.
    fn atom(&mut self) -> String {
        let Some(c) = self.peek() else {
            return String::new();
        };
        self.at += 1;
        match c {
            '{' => self.sequence(true),
            '}' => String::new(),
            '\\' => self.command(),
            '~' => " ".to_owned(),
            '\'' => "′".to_owned(),
            '&' => " ".to_owned(),
            c => c.to_string(),
        }
    }

    /// A command's or script's argument: a group or one atom, spaces before it skipped.
    fn argument(&mut self) -> String {
        while self.peek() == Some(' ') {
            self.at += 1;
        }
        self.atom()
    }

    /// The control sequence after a backslash.
    fn command(&mut self) -> String {
        let start = self.at;
        while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            self.at += 1;
        }
        if self.at == start {
            // A control symbol: `\,`, `\{`, `\\`, `\|`, …
            let Some(c) = self.peek() else {
                return String::new();
            };
            self.at += 1;
            return match c {
                ',' | ':' | ';' | '>' => " ".to_owned(),
                '!' => String::new(),
                '\\' => " ".to_owned(),
                '|' => "‖".to_owned(),
                c => c.to_string(),
            };
        }
        let name: String = self.chars[start..self.at].iter().collect();
        if let Some((_, symbol)) = COMMANDS.iter().find(|(n, _)| *n == name) {
            return (*symbol).to_owned();
        }
        if WORDS.contains(&name.as_str()) {
            return format!(" {name} ");
        }
        if SILENT.contains(&name.as_str()) {
            return String::new();
        }
        if TRANSPARENT.contains(&name.as_str()) {
            return self.argument();
        }
        match name.as_str() {
            "frac" | "dfrac" | "tfrac" | "cfrac" => {
                let (top, bottom) = (self.argument(), self.argument());
                format!("{}/{}", wrapped(&top), wrapped(&bottom))
            }
            "binom" => {
                let (n, k) = (self.argument(), self.argument());
                format!("C({n}, {k})")
            }
            "sqrt" => {
                if self.peek() == Some('[') {
                    let degree: String = self.until(']');
                    let body = self.argument();
                    return format!("{}√{}", scripted(&degree, true), wrapped(&body));
                }
                format!("√{}", wrapped(&self.argument()))
            }
            "mathbb" => self
                .argument()
                .chars()
                .map(|c| {
                    BLACKBOARD
                        .iter()
                        .find(|(l, _)| *l == c)
                        .map_or(c, |(_, b)| *b)
                })
                .collect(),
            "begin" | "end" => {
                self.argument();
                String::new()
            }
            other => other.to_owned(),
        }
    }

    /// The characters up to `close` (consumed), after an opening one.
    fn until(&mut self, close: char) -> String {
        self.at += 1;
        let mut out = String::new();
        while let Some(c) = self.peek() {
            self.at += 1;
            if c == close {
                break;
            }
            out.push(c);
        }
        out
    }
}

/// `text` raised or lowered: script characters when each has one, else `^(text)`/`_(text)`.
fn scripted(text: &str, up: bool) -> String {
    let text = text.trim();
    let table = if up { SUPERSCRIPTS } else { SUBSCRIPTS };
    let mapped: Option<String> = text
        .chars()
        .map(|c| table.iter().find(|(from, _)| *from == c).map(|(_, to)| *to))
        .collect();
    match mapped {
        Some(mapped) if !mapped.is_empty() => mapped,
        _ if text.chars().count() == 1 => format!("{}{text}", if up { '^' } else { '_' }),
        _ => format!("{}({text})", if up { '^' } else { '_' }),
    }
}

/// `text` in parentheses unless it is one character or word.
fn wrapped(text: &str) -> String {
    let text = text.trim();
    if text.chars().all(char::is_alphanumeric) {
        text.to_owned()
    } else {
        format!("({text})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tex_reads_as_unicode() {
        assert_eq!(approximate("h_i"), "hᵢ");
        assert_eq!(approximate("x^2 + y^{10}"), "x² + y¹⁰");
        assert_eq!(approximate(r"\alpha \le \beta"), "α ≤ β");
        assert_eq!(approximate(r"\frac{a}{b+1}"), "a/(b+1)");
        assert_eq!(approximate(r"\sqrt{x}"), "√x");
        assert_eq!(approximate(r"O(n \log n)"), "O(n log n)");
        assert_eq!(approximate(r"\mathbb{R}^n"), "ℝⁿ");
        assert_eq!(approximate(r"\left(h_i' - h_i\right)"), "(hᵢ′ - hᵢ)");
        assert_eq!(approximate(r"\sum_{i=1}^{n} x_{ij}"), "∑ᵢ₌₁ⁿ xᵢⱼ");
        assert_eq!(approximate(r"x_{\text{max}}"), "xₘₐₓ");
        assert_eq!(approximate(r"x_{\beta}"), "x_β");
        assert_eq!(approximate(r"\text{if } x"), "if x");
    }

    #[test]
    fn escaping_keeps_markdown_punctuation_literal() {
        let mut out = String::new();
        escaped("a*b_c|d", &mut out);
        assert_eq!(out, r"a\*b\_c\|d");
    }
}
