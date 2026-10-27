//! Right-to-left text on the primary screen: the visual order of a line's
//! cells (the implicit part of the Unicode bidi algorithm, UAX #9, for a
//! left-to-right paragraph without explicit embeddings) and Arabic
//! contextual letter forms (Presentation Forms). Cells stay in logical order
//! in the grid; only drawing and mouse hit-testing go through the order.

/// Bidi class of a cell's (base) character, reduced to what the implicit
/// rules need.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Class {
    /// Left-to-right letter.
    L,
    /// Right-to-left letter (Hebrew, Arabic, Syriac, Thaana, N'Ko, ...).
    R,
    /// European digit.
    En,
    /// Arabic-Indic digit.
    An,
    /// Everything else: spaces, punctuation, symbols, empty cells.
    N,
}

/// Whether `c` is a strong right-to-left character.
pub fn is_rtl(c: char) -> bool {
    matches!(c as u32, 0x0590..=0x05FF | 0x0600..=0x065F | 0x066A..=0x06EF | 0x06FA..=0x08FF
        | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x10800..=0x10FFF | 0x1E800..=0x1EFFF)
}

fn class(c: char) -> Class {
    match c as u32 {
        0x30..=0x39 | 0x06F0..=0x06F9 => Class::En,
        0x0660..=0x0669 => Class::An,
        _ if is_rtl(c) => Class::R,
        _ if c.is_alphabetic() => Class::L,
        _ => Class::N,
    }
}

/// Fill `order` with the logical column shown at each visual column of a
/// line whose cells hold `chars` (one per cell, a wide character's second
/// cell holds `'\0'` and stays to the right of it). `levels` is scratch
/// space. Returns false (and leaves `order` empty) when the line has no
/// right-to-left text, so the identity order applies.
pub fn visual_order(chars: &[char], levels: &mut Vec<u8>, order: &mut Vec<u16>) -> bool {
    order.clear();
    if !chars.iter().any(|&c| is_rtl(c)) {
        return false;
    }
    let n = chars.len();
    levels.clear();
    levels.resize(n, 0);
    // W rules + I1: R -> 1, digits after a right-to-left letter -> 2 (else
    // they are left-to-right, level 0), L -> 0. Neutrals resolved below.
    let mut last_strong = Class::L;
    let classes: Vec<Class> = chars
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            // A wide character's second cell goes with it.
            if c == '\0' && i > 0 && chars[i - 1] != '\0' && crate::grid::char_width(chars[i - 1]) == 2 {
                return Class::N;
            }
            let k = class(c);
            match k {
                Class::L | Class::R => last_strong = k,
                Class::En if last_strong == Class::L => return Class::L,
                _ => {}
            }
            k
        })
        .collect();
    for (lvl, k) in levels.iter_mut().zip(&classes) {
        *lvl = match k {
            Class::R => 1,
            Class::En | Class::An => 2,
            _ => 0,
        };
    }
    // N1 / N2: a run of neutrals between two right-to-left sides (digits
    // count as right-to-left) is right-to-left, else it takes the paragraph's
    // left-to-right level.
    let rtl_side = |k: Class| matches!(k, Class::R | Class::En | Class::An);
    let mut i = 0;
    while i < n {
        if classes[i] != Class::N {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && classes[i] == Class::N {
            i += 1;
        }
        let before = start.checked_sub(1).map(|j| classes[j]);
        let after = (i < n).then(|| classes[i]);
        if before.is_some_and(rtl_side) && after.is_some_and(rtl_side) {
            levels[start..i].fill(1);
        }
    }
    // Keep a wide character's two cells in order: same level as the first.
    for j in 1..n {
        if chars[j] == '\0' && chars[j - 1] != '\0' && crate::grid::char_width(chars[j - 1]) == 2 {
            levels[j] = levels[j - 1];
        }
    }
    // L2: reverse every run at level >= k, from the highest level down to 1.
    order.extend(0..n as u16);
    for k in (1..=2).rev() {
        let mut v = 0;
        while v < n {
            if levels[order[v] as usize] < k {
                v += 1;
                continue;
            }
            let start = v;
            while v < n && levels[order[v] as usize] >= k {
                v += 1;
            }
            order[start..v].reverse();
        }
    }
    // Reversal swapped wide pairs; put each first cell back on the left.
    for v in 1..n {
        let (a, b) = (order[v - 1] as usize, order[v] as usize);
        if a == b + 1 && chars[a] == '\0' {
            order.swap(v - 1, v);
        }
    }
    true
}

/// Arabic letters with contextual forms: the isolated form's codepoint and
/// whether the letter joins on both sides (else only to the previous letter).
fn arabic_forms(c: char) -> Option<(u32, bool)> {
    Some(match c as u32 {
        0x0622 => (0xFE81, false),
        0x0623 => (0xFE83, false),
        0x0624 => (0xFE85, false),
        0x0625 => (0xFE87, false),
        0x0626 => (0xFE89, true),
        0x0627 => (0xFE8D, false),
        0x0628 => (0xFE8F, true),
        0x0629 => (0xFE93, false),
        0x062A => (0xFE95, true),
        0x062B => (0xFE99, true),
        0x062C => (0xFE9D, true),
        0x062D => (0xFEA1, true),
        0x062E => (0xFEA5, true),
        0x062F => (0xFEA9, false),
        0x0630 => (0xFEAB, false),
        0x0631 => (0xFEAD, false),
        0x0632 => (0xFEAF, false),
        0x0633 => (0xFEB1, true),
        0x0634 => (0xFEB5, true),
        0x0635 => (0xFEB9, true),
        0x0636 => (0xFEBD, true),
        0x0637 => (0xFEC1, true),
        0x0638 => (0xFEC5, true),
        0x0639 => (0xFEC9, true),
        0x063A => (0xFECD, true),
        0x0641 => (0xFED1, true),
        0x0642 => (0xFED5, true),
        0x0643 => (0xFED9, true),
        0x0644 => (0xFEDD, true),
        0x0645 => (0xFEE1, true),
        0x0646 => (0xFEE5, true),
        0x0647 => (0xFEE9, true),
        0x0648 => (0xFEED, false),
        0x0649 => (0xFEEF, false),
        0x064A => (0xFEF1, true),
        // Persian / Urdu.
        0x067E => (0xFB56, true),
        0x0686 => (0xFB7A, true),
        0x0698 => (0xFB8A, false),
        0x06A9 => (0xFB8E, true),
        0x06AF => (0xFB92, true),
        0x06CC => (0xFBFC, true),
        _ => return None,
    })
}

/// Whether `c` connects to the letter after it (dual-joining or tatweel).
fn joins_next(c: Option<char>) -> bool {
    c.is_some_and(|c| c == '\u{0640}' || arabic_forms(c).is_some_and(|(_, dual)| dual))
}

/// Whether `c` connects to the letter before it.
fn joins_prev(c: Option<char>) -> bool {
    c.is_some_and(|c| c == '\u{0640}' || arabic_forms(c).is_some())
}

/// The contextual form of Arabic letter `c` between its logical neighbours
/// (isolated, final, initial or medial), or `c` itself for other characters.
pub fn arabic_form(prev: Option<char>, c: char, next: Option<char>) -> char {
    let Some((iso, dual)) = arabic_forms(c) else { return c };
    let from_prev = joins_next(prev);
    let to_next = dual && joins_prev(next);
    let form = match (from_prev, to_next) {
        (false, false) => 0,
        (true, false) => 1,
        (false, true) => 2,
        (true, true) => 3,
    };
    char::from_u32(iso + form).unwrap_or(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order_of(s: &str) -> Option<String> {
        let chars: Vec<char> = s.chars().collect();
        let mut order = Vec::new();
        visual_order(&chars, &mut Vec::new(), &mut order).then(|| order.iter().map(|&i| chars[i as usize]).collect())
    }

    #[test]
    fn ltr_lines_keep_their_order() {
        assert_eq!(order_of("hello 123"), None);
    }

    #[test]
    fn rtl_runs_are_reversed_with_digits_kept_left_to_right() {
        // Hebrew "shalom" (ש ל ו ם) shows right to left.
        assert_eq!(order_of("a שלום b").unwrap(), "a םולש b");
        // Two Hebrew words keep the space between them inside the run.
        assert_eq!(order_of("אב גד").unwrap(), "דג בא");
        // Digits inside a right-to-left run stay in reading order.
        assert_eq!(order_of("אב 12 גד").unwrap(), "דג 12 בא");
        // Trailing spaces stay at the end of the line.
        assert_eq!(order_of("אב  ").unwrap(), "בא  ");
    }

    #[test]
    fn wide_characters_keep_their_two_cells_in_order() {
        let chars = ['א', 'ב', '漢', '\0'];
        let mut order = Vec::new();
        assert!(visual_order(&chars, &mut Vec::new(), &mut order));
        assert_eq!(order, [1, 0, 2, 3]);
    }

    #[test]
    fn arabic_letters_take_contextual_forms() {
        // سلام: seen initial, lam medial, alef final (alef never joins on), meem isolated.
        let w: Vec<char> = "سلام".chars().collect();
        let form = |i: usize| arabic_form(i.checked_sub(1).map(|j| w[j]), w[i], w.get(i + 1).copied());
        assert_eq!([form(0), form(1), form(2), form(3)], ['\u{FEB3}', '\u{FEE0}', '\u{FE8E}', '\u{FEE1}']);
        assert_eq!(arabic_form(None, 'x', None), 'x');
    }
}
