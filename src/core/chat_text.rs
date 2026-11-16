//! Chat text as the game shows it: the styled runs the agent sends, and `§` formatting codes in plain text.

use serde::{Deserialize, Serialize};

const PALETTE: [u32; 16] = [0x000000, 0x0000AA, 0x00AA00, 0x00AAAA, 0xAA0000, 0xAA00AA, 0xFFAA00, 0xAAAAAA, 0x555555, 0x5555FF, 0x55FF55, 0x55FFFF, 0xFF5555, 0xFF55FF, 0xFFFF55, 0xFFFFFF];
const MAX_RUNS: usize = 200;

/// One stretch of chat text in one style.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    #[serde(rename = "t")]
    pub text: String,
    /// The colour as `0xRRGGBB`; `None` is the default colour.
    #[serde(rename = "c", default, skip_serializing_if = "Option::is_none")]
    pub color: Option<u32>,
    #[serde(rename = "b", default, skip_serializing_if = "is_false")]
    pub bold: bool,
    #[serde(rename = "i", default, skip_serializing_if = "is_false")]
    pub italic: bool,
    #[serde(rename = "u", default, skip_serializing_if = "is_false")]
    pub underline: bool,
    #[serde(rename = "s", default, skip_serializing_if = "is_false")]
    pub strike: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Run {
    fn same_style(&self, other: &Run) -> bool {
        (self.color, self.bold, self.italic, self.underline, self.strike) == (other.color, other.bold, other.italic, other.underline, other.strike)
    }

    fn styled_like(&self) -> Run {
        Run { text: String::new(), ..self.clone() }
    }
}

/// The runs to show for a line: `runs` from the agent, else `text`, with `§` codes in either turned into styles,
/// obfuscated text shown plain and neighbours of one style joined.
pub fn styled(runs: &[Run], text: &str) -> Vec<Run> {
    let mut out = Vec::new();
    if runs.is_empty() {
        legacy(&Run { text: text.to_owned(), ..Run::default() }, &mut out);
    } else {
        for run in runs.iter().take(MAX_RUNS) {
            legacy(run, &mut out);
        }
    }
    out
}

/// The text of `runs` without any styling.
pub fn plain(runs: &[Run]) -> String {
    runs.iter().map(|r| r.text.as_str()).collect()
}

/// Whether a line needs runs to show as the game does: styled runs came, or the text holds `§` codes.
pub fn needs_runs(runs: &[Run], text: &str) -> bool {
    !runs.is_empty() || text.contains('§')
}

fn push(out: &mut Vec<Run>, run: &mut Run) {
    if run.text.is_empty() {
        return;
    }
    let text = std::mem::take(&mut run.text);
    match out.last_mut() {
        Some(last) if last.same_style(run) => last.text.push_str(&text),
        _ => out.push(Run { text, ..run.clone() }),
    }
}

fn legacy(base: &Run, out: &mut Vec<Run>) {
    let mut now = base.styled_like();
    let mut chars = base.text.chars();
    while let Some(c) = chars.next() {
        if c != '§' {
            now.text.push(c);
            continue;
        }
        let Some(code) = chars.next().map(|c| c.to_ascii_lowercase()) else { break };
        push(out, &mut now);
        match code {
            'l' => now.bold = true,
            'o' => now.italic = true,
            'n' => now.underline = true,
            'm' => now.strike = true,
            'r' => now = base.styled_like(),
            'x' => {
                let hex: String = (0..6).filter_map(|_| chars.next().filter(|c| *c == '§').and_then(|_| chars.next())).collect();
                if let Ok(rgb) = u32::from_str_radix(&hex, 16)
                    && hex.len() == 6
                {
                    now = Run { color: Some(rgb), ..Run::default() };
                }
            }
            c => {
                if let Some(i) = c.to_digit(16) {
                    now = Run { color: PALETTE.get(i as usize).copied(), ..Run::default() };
                }
            }
        }
    }
    push(out, &mut now);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, color: Option<u32>) -> Run {
        Run { text: text.into(), color, ..Run::default() }
    }

    #[test]
    fn plain_text_is_one_run() {
        assert_eq!(styled(&[], "hallo"), [run("hallo", None)]);
        assert!(!needs_runs(&[], "hallo"));
        assert!(needs_runs(&[], "§ehallo"));
    }

    #[test]
    fn section_codes_become_styles() {
        let out = styled(&[], "§6[VIP] §lNotch§r: hi §kxx");
        assert_eq!(out[0], run("[VIP] ", Some(0xFFAA00)));
        assert_eq!(out[1], Run { text: "Notch".into(), color: Some(0xFFAA00), bold: true, ..Run::default() });
        assert_eq!(out[2], run(": hi xx", None));
        assert_eq!(plain(&out), "[VIP] Notch: hi xx");
    }

    #[test]
    fn a_colour_code_resets_the_format() {
        let out = styled(&[], "§l§obold§cred");
        assert!(out[0].bold && out[0].italic);
        assert_eq!(out[1], run("red", Some(0xFF5555)));
    }

    #[test]
    fn hex_codes_and_broken_codes_are_safe() {
        assert_eq!(styled(&[], "§x§1§2§a§b§c§dhi"), [run("hi", Some(0x12ABCD))]);
        assert_eq!(plain(&styled(&[], "end §")), "end ");
        assert_eq!(plain(&styled(&[], "§zx§")), "x");
        assert_eq!(plain(&styled(&[], "ä§aö")), "äö");
    }

    #[test]
    fn agent_runs_keep_their_style_and_join_neighbours() {
        let runs = vec![run("<", None), run("Notch", None), Run { text: "> §chi".into(), italic: true, ..Run::default() }];
        let out = styled(&runs, "hi");
        assert_eq!(out[0], run("<Notch", None));
        assert_eq!(out[1], Run { text: "> ".into(), italic: true, ..Run::default() });
        assert_eq!(out[2], run("hi", Some(0xFF5555)));
    }

    #[test]
    fn runs_parse_from_agent_json() {
        let r: Run = serde_json::from_str(r#"{"t":"x","c":16755200,"b":true}"#).unwrap();
        assert_eq!(r, Run { text: "x".into(), color: Some(0xFFAA00), bold: true, ..Run::default() });
        assert_eq!(serde_json::to_string(&run("y", None)).unwrap(), r#"{"t":"y"}"#);
    }
}
