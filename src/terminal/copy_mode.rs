//! The label picker behind copy mode.
//!
//! `Ctrl+Shift+C` scans the visible screen, puts a two-letter label over every
//! URL, path and word, and copies whichever one you spell. It is the keyboard
//! answer to reaching for the mouse: the buffer is only read, never changed,
//! and the labels are recomputed when the mode is entered.
//!
//! The matching and labelling are pure — [`CopyLine`] is all this needs from
//! the grid — so the interesting half is testable without a terminal. The
//! view owns the overlay and the clipboard write.

use regex::Regex;
use std::sync::OnceLock;

use crate::ui::i18n::{L10nKey, t, t_fmt};

/// xterm-muxer's quick-select pattern, which in turn mirrors wezterm's
/// built-ins: URLs, paths with at least one separator, then any word. The
/// order matters — a path is offered as a path rather than as the last of its
/// components, and a URL as one address rather than as a host and a tail.
const PATTERN: &str =
    r"(?:https?://|git@|git://|ssh://|ftp://|file://)\S+|(?:[.\w\-@~]+)?(?:/+[.\w\-@]+)+|[\w./-]+";

/// Every match on screen gets exactly this many letters, so the choice is
/// always the same gesture: two keys, no Enter. The xterm-muxer/wezterm
/// quick-select table drops to one letter for the first 26, which makes the
/// first key ambiguous between "a" and "ab"; this mode keeps the width fixed
/// and reserves the shorter spellings.
pub(super) const LABEL_WIDTH: usize = 2;

const ALPHABET: &[u8; 26] = b"abcdefghijklmnopqrstuvwxyz";

/// One visible row, with each character's cell column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CopyLine {
    /// Screen row, `0` at the top.
    pub row: usize,
    pub text: String,
    /// `columns[i]` is the terminal column of `text[i]`. A wide glyph occupies
    /// two cells but appears once here, mapped to its leading column.
    pub columns: Vec<usize>,
}

/// One labelled thing on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CopyMatch {
    pub text: String,
    pub row: usize,
    pub col: usize,
    /// Width in cells, which is what the overlay paints.
    pub width: usize,
    pub label: String,
}

/// What a typed label letter did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CopyOutcome {
    /// The letter is a prefix of at least one label; keep waiting.
    Pending,
    /// A whole label: the index of the match to copy.
    Commit(usize),
    /// Nothing on screen begins with what has been typed.
    NoMatch,
}

/// The live mode: what was found when it opened, and what has been typed.
#[derive(Clone, Debug, Default)]
pub(super) struct CopyModeState {
    pub matches: Vec<CopyMatch>,
    pub prefix: String,
}

impl CopyModeState {
    pub fn new(matches: Vec<CopyMatch>) -> Self {
        Self {
            matches,
            prefix: String::new(),
        }
    }

    /// Feed one typed character. Uppercase is folded: the labels never use
    /// it, and a Shift held by habit must not make the picker go dead.
    pub fn type_char(&mut self, ch: char) -> CopyOutcome {
        let ch = ch.to_ascii_lowercase();
        if !ch.is_ascii_lowercase() {
            self.prefix.clear();
            return CopyOutcome::NoMatch;
        }
        if self.matches.is_empty() {
            return CopyOutcome::NoMatch;
        }
        let mut candidate = self.prefix.clone();
        candidate.push(ch);
        if let Some(index) = self
            .matches
            .iter()
            .position(|m| m.label.eq_ignore_ascii_case(&candidate))
        {
            return CopyOutcome::Commit(index);
        }
        if self.matches.iter().any(|m| m.label.starts_with(&candidate)) {
            self.prefix = candidate;
            CopyOutcome::Pending
        } else {
            self.prefix.clear();
            CopyOutcome::NoMatch
        }
    }

    pub fn backspace(&mut self) {
        self.prefix.pop();
    }

    /// The hint's middle word: what has been typed so far, or a placeholder
    /// while the first key is still to come.
    pub fn prefix_label(&self) -> &str {
        &self.prefix
    }
}

/// Find everything labelable in `lines`, in reading order.
pub(super) fn find_matches(lines: &[CopyLine]) -> Vec<CopyMatch> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(PATTERN).expect("the copy-mode pattern compiles"));
    let mut out = Vec::new();
    for line in lines {
        // The matcher indexes bytes; `columns` is indexed by character, so
        // each match is converted once. Lines are one screen row long, which
        // keeps the recount cheap.
        for m in re.find_iter(&line.text) {
            if m.start() == m.end() {
                continue;
            }
            let start_char = line.text[..m.start()].chars().count();
            let end_char = start_char + m.as_str().chars().count();
            let Some(&col) = line.columns.get(start_char) else {
                continue;
            };
            let Some(&last_col) = line.columns.get(end_char.saturating_sub(1)) else {
                continue;
            };
            // Wide glyphs leave a spacer cell: the run is as wide as the
            // columns it spans, not as the characters it has.
            let width = last_col.saturating_sub(col) + 1;
            out.push(CopyMatch {
                text: m.as_str().to_string(),
                row: line.row,
                col,
                width: width.max(1),
                label: String::new(),
            });
        }
    }
    assign_labels(&mut out);
    out
}

/// Give every match its two-letter label. Past `26²` matches — a wall of
/// text on a very wide screen — the width grows rather than repeating a
/// label, so a typed pair still names at most one thing.
fn assign_labels(matches: &mut [CopyMatch]) {
    if matches.is_empty() {
        return;
    }
    let mut width = LABEL_WIDTH;
    while 26usize.saturating_pow(width as u32) < matches.len() {
        width += 1;
    }
    for (i, m) in matches.iter_mut().enumerate() {
        let mut value = i;
        let mut label = vec![b'a'; width];
        for slot in label.iter_mut().rev() {
            *slot = ALPHABET[value % ALPHABET.len()];
            value /= ALPHABET.len();
        }
        m.label = String::from_utf8(label).expect("labels are ascii");
    }
}

/// The hint under the overlay.
pub(super) fn hint_text(state: &CopyModeState) -> String {
    if state.matches.is_empty() {
        return t(L10nKey::CopyModeNoMatches).to_string();
    }
    match state.prefix.is_empty() {
        true => t(L10nKey::CopyModeHint).to_string(),
        false => t_fmt(
            L10nKey::CopyModeHintTyped,
            &[("prefix", state.prefix_label())],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(row: usize, text: &str) -> CopyLine {
        CopyLine {
            row,
            text: text.to_string(),
            columns: (0..text.chars().count()).collect(),
        }
    }

    #[test]
    fn urls_paths_and_words_are_all_offered_once() {
        let matches = find_matches(&[line(0, "see /usr/local/bin/tool and https://x.y/z now")]);
        let texts: Vec<&str> = matches.iter().map(|m| m.text.as_str()).collect();
        assert!(texts.contains(&"/usr/local/bin/tool"));
        assert!(texts.contains(&"https://x.y/z"));
        assert!(texts.contains(&"see"));
        assert!(texts.contains(&"now"));
        // The path is one label, not one per component.
        assert_eq!(
            texts
                .iter()
                .filter(|t| **t == "/usr/local/bin/tool")
                .count(),
            1
        );
    }

    #[test]
    fn labels_are_always_two_letters_and_unique() {
        let matches = find_matches(&[line(0, "a b c d e f g h i j k l m n o p")]);
        assert!(matches.len() > 2);
        assert!(matches.iter().all(|m| m.label.len() == LABEL_WIDTH));
        let mut labels: Vec<&str> = matches.iter().map(|m| m.label.as_str()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), matches.len());
    }

    #[test]
    fn a_typed_pair_commits_exactly_one_match() {
        let matches = find_matches(&[line(0, "alpha beta gamma delta")]);
        let mut state = CopyModeState::new(matches);
        let first = state.matches[1].label.chars().next().unwrap();
        assert_eq!(state.type_char(first), CopyOutcome::Pending);
        let second = state.matches[1].label.chars().nth(1).unwrap();
        assert_eq!(state.type_char(second), CopyOutcome::Commit(1));
    }

    #[test]
    fn a_letter_that_begins_nothing_clears_the_prefix() {
        let matches = find_matches(&[line(0, "alpha beta")]);
        let mut state = CopyModeState::new(matches);
        let first = state.matches[0].label.chars().next().unwrap();
        assert_eq!(state.type_char(first), CopyOutcome::Pending);
        assert_eq!(state.type_char('9'), CopyOutcome::NoMatch);
        assert!(state.prefix.is_empty());
    }

    #[test]
    fn wide_glyphs_take_their_cells() {
        let mut l = line(0, "a界b");
        // The grid reports the wide glyph's leading cell only; its spacer is
        // skipped, so the character columns are 0, 1, 3.
        l.columns = vec![0, 1, 3];
        let matches = find_matches(&[l]);
        let word = matches.iter().find(|m| m.text == "a界b").unwrap();
        assert_eq!(word.col, 0);
        assert_eq!(word.width, 4);
    }
}
