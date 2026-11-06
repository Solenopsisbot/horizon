//! A bounded `java.util.regex` subset for whole-input matching.
//!
//! Mods validate ids and names with `String.matches("[a-z0-9_.-]*")` and
//! `Pattern.matches` before they register anything (supermartijn642's core
//! library refuses its own mod id without it). The runtime takes no regex
//! dependency, so this module implements the part of Java's syntax those
//! validators use, exactly, and answers `None` for anything else so the caller
//! can name the pattern instead of guessing a result:
//!
//! - literals and escaped metacharacters (`\.`, `\-`, `\\`, ...), `\t \n \r \f`;
//! - `.` (any character except a line terminator, Java's default);
//! - the predefined classes `\d \D \w \W \s \S` (ASCII, Java's default);
//! - bracket classes `[...]`, negated `[^...]`, ranges, and escapes and
//!   predefined classes inside them (not intersections `&&` or nested classes);
//! - groups `(...)`, `(?:...)`, alternation `|`;
//! - greedy quantifiers `* + ? {n} {n,} {n,m}` and their lazy forms (for a
//!   whole-input match the two accept the same inputs);
//! - `^` at the start and `$` at the end.
//!
//! Matching is a backtracking search over the whole input with a step budget,
//! so a pathological pattern stops with `None` instead of running unbounded.

use std::cell::Cell;

/// Patterns longer than this are not compiled; real validators are short.
const MAX_PATTERN_CHARS: usize = 512;
/// Inputs longer than this are not matched: the search is bounded per call.
const MAX_INPUT_CHARS: usize = 4_096;
/// Backtracking steps one match may spend before it gives up.
const MAX_STEPS: usize = 200_000;
/// The largest explicit repetition count `{n,m}` accepted.
const MAX_REPEAT: usize = 1_000;

#[derive(Debug, Clone, PartialEq)]
enum ClassItem {
    Char(char),
    Range(char, char),
    Digit(bool),
    Word(bool),
    Space(bool),
}

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Char(char),
    Any,
    Class {
        negated: bool,
        items: Vec<ClassItem>,
    },
    Group(Vec<Vec<Node>>),
    Repeat {
        node: Box<Node>,
        min: usize,
        max: Option<usize>,
    },
}

/// Whether `input` matches `pattern` in full, as `String.matches` answers.
///
/// `None` means the pattern uses syntax outside the supported subset, or the
/// bounded search ran out of budget; the caller reports that rather than
/// inventing an answer.
pub(crate) fn full_match(pattern: &str, input: &str) -> Option<bool> {
    let pattern: Vec<char> = pattern.chars().collect();
    let input: Vec<char> = input.chars().collect();
    if pattern.len() > MAX_PATTERN_CHARS || input.len() > MAX_INPUT_CHARS {
        return None;
    }
    let mut parser = Parser {
        chars: &pattern,
        at: 0,
    };
    let alternatives = parser.alternation(0)?;
    if parser.at != pattern.len() {
        return None;
    }
    let search = Search {
        input: &input,
        steps: Cell::new(0),
    };
    let end = input.len();
    search.node(&Node::Group(alternatives), 0, &mut |at| Some(at == end))
}

struct Parser<'a> {
    chars: &'a [char],
    at: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn next(&mut self) -> Option<char> {
        let value = self.peek()?;
        self.at += 1;
        Some(value)
    }

    /// `a|b|c` up to a closing `)` (depth > 0) or the end.
    fn alternation(&mut self, depth: usize) -> Option<Vec<Vec<Node>>> {
        if depth > 32 {
            return None;
        }
        let mut alternatives = vec![self.sequence(depth)?];
        while self.peek() == Some('|') {
            self.at += 1;
            alternatives.push(self.sequence(depth)?);
        }
        Some(alternatives)
    }

    fn sequence(&mut self, depth: usize) -> Option<Vec<Node>> {
        let mut nodes = Vec::new();
        while let Some(value) = self.peek() {
            match value {
                '|' => break,
                ')' if depth > 0 => break,
                ')' => return None,
                '^' if self.at == 0 => {
                    self.at += 1;
                    continue;
                }
                '$' if self.at + 1 == self.chars.len() => {
                    self.at += 1;
                    continue;
                }
                _ => {}
            }
            let atom = self.atom(depth)?;
            let atom = self.quantified(atom)?;
            nodes.push(atom);
        }
        Some(nodes)
    }

    fn atom(&mut self, depth: usize) -> Option<Node> {
        match self.next()? {
            '.' => Some(Node::Any),
            '[' => self.class(),
            '(' => {
                if self.peek() == Some('?') {
                    self.at += 1;
                    if self.next()? != ':' {
                        return None;
                    }
                }
                let alternatives = self.alternation(depth + 1)?;
                (self.next()? == ')').then_some(Node::Group(alternatives))
            }
            '\\' => match self.escape()? {
                ClassItem::Char(value) => Some(Node::Char(value)),
                item => Some(Node::Class {
                    negated: false,
                    items: vec![item],
                }),
            },
            '*' | '+' | '?' | '{' | '^' | '$' | ']' | '}' => None,
            value => Some(Node::Char(value)),
        }
    }

    /// The character after a `\`.
    fn escape(&mut self) -> Option<ClassItem> {
        let value = self.next()?;
        Some(match value {
            'd' => ClassItem::Digit(true),
            'D' => ClassItem::Digit(false),
            'w' => ClassItem::Word(true),
            'W' => ClassItem::Word(false),
            's' => ClassItem::Space(true),
            'S' => ClassItem::Space(false),
            't' => ClassItem::Char('\t'),
            'n' => ClassItem::Char('\n'),
            'r' => ClassItem::Char('\r'),
            'f' => ClassItem::Char('\u{000C}'),
            // Any other letter or digit escape is a construct this subset does
            // not model (`\b`, `\p{...}`, back references, ...).
            value if value.is_ascii_alphanumeric() => return None,
            value => ClassItem::Char(value),
        })
    }

    fn class(&mut self) -> Option<Node> {
        let negated = self.peek() == Some('^');
        if negated {
            self.at += 1;
        }
        let mut items = Vec::new();
        let mut first = true;
        loop {
            let value = self.next()?;
            match value {
                ']' if !first => break,
                '[' => return None,
                '&' if self.peek() == Some('&') => return None,
                _ => {}
            }
            first = false;
            let item = if value == '\\' {
                self.escape()?
            } else {
                ClassItem::Char(value)
            };
            // `a-z` is a range unless the `-` closes the class (`[a-]`).
            if let ClassItem::Char(start) = item
                && self.peek() == Some('-')
                && self.chars.get(self.at + 1).is_some_and(|next| *next != ']')
            {
                self.at += 1;
                let end = match self.next()? {
                    '\\' => match self.escape()? {
                        ClassItem::Char(end) => end,
                        _ => return None,
                    },
                    end => end,
                };
                if end < start {
                    return None;
                }
                items.push(ClassItem::Range(start, end));
            } else {
                items.push(item);
            }
        }
        Some(Node::Class { negated, items })
    }

    fn quantified(&mut self, atom: Node) -> Option<Node> {
        let (min, max) = match self.peek() {
            Some('*') => (0, None),
            Some('+') => (1, None),
            Some('?') => (0, Some(1)),
            Some('{') => {
                self.at += 1;
                let min = self.number()?;
                let max = if self.peek() == Some(',') {
                    self.at += 1;
                    if self.peek() == Some('}') {
                        None
                    } else {
                        Some(self.number()?)
                    }
                } else {
                    Some(min)
                };
                if self.next()? != '}' || max.is_some_and(|max| max < min) {
                    return None;
                }
                // Step back one so the shared advance below consumes nothing.
                self.at -= 1;
                (min, max)
            }
            _ => return Some(atom),
        };
        self.at += 1;
        match self.peek() {
            // Lazy and greedy accept the same inputs for a whole-input match.
            Some('?') => self.at += 1,
            // Possessive quantifiers can reject inputs a backtracking match
            // accepts; they are outside the subset.
            Some('+') => return None,
            _ => {}
        }
        Some(Node::Repeat {
            node: Box::new(atom),
            min,
            max,
        })
    }

    fn number(&mut self) -> Option<usize> {
        let start = self.at;
        while self.peek().is_some_and(|value| value.is_ascii_digit()) {
            self.at += 1;
        }
        let text: String = self.chars[start..self.at].iter().collect();
        text.parse().ok().filter(|value| *value <= MAX_REPEAT)
    }
}

fn class_item_matches(item: &ClassItem, value: char) -> bool {
    match item {
        ClassItem::Char(expected) => *expected == value,
        ClassItem::Range(start, end) => (*start..=*end).contains(&value),
        ClassItem::Digit(positive) => value.is_ascii_digit() == *positive,
        ClassItem::Word(positive) => (value.is_ascii_alphanumeric() || value == '_') == *positive,
        ClassItem::Space(positive) => {
            matches!(value, ' ' | '\t' | '\n' | '\u{000B}' | '\u{000C}' | '\r') == *positive
        }
    }
}

/// The single-character test of a leaf node.
fn leaf_matches(node: &Node, value: char) -> bool {
    match node {
        Node::Char(expected) => *expected == value,
        // Java's `.` excludes line terminators unless DOTALL is set.
        Node::Any => !matches!(value, '\n' | '\r' | '\u{0085}' | '\u{2028}' | '\u{2029}'),
        Node::Class { negated, items } => {
            items.iter().any(|item| class_item_matches(item, value)) != *negated
        }
        Node::Group(_) | Node::Repeat { .. } => false,
    }
}

/// One bounded backtracking search over a fixed input.
///
/// The step counter is a `Cell` so continuations (closures that already
/// borrow the search) can keep spending from it.
struct Search<'a> {
    input: &'a [char],
    steps: Cell<usize>,
}

/// A continuation: given an end position, whether the rest of the pattern
/// matches from there. `None` is budget exhaustion.
type Next<'n> = &'n mut dyn FnMut(usize) -> Option<bool>;

impl Search<'_> {
    fn spend(&self) -> Option<()> {
        let steps = self.steps.get() + 1;
        self.steps.set(steps);
        (steps <= MAX_STEPS).then_some(())
    }

    /// Match `node` at `at`, calling `next` with every end position it can
    /// reach (greedy order first) until one answers `true`.
    fn node(&self, node: &Node, at: usize, next: Next<'_>) -> Option<bool> {
        self.spend()?;
        match node {
            Node::Group(alternatives) => {
                for sequence in alternatives {
                    if self.sequence(sequence, at, next)? {
                        return Some(true);
                    }
                }
                Some(false)
            }
            Node::Repeat { node, min, max } => self.repeat(node, *min, *max, 0, at, next),
            leaf => {
                if self
                    .input
                    .get(at)
                    .is_some_and(|value| leaf_matches(leaf, *value))
                {
                    next(at + 1)
                } else {
                    Some(false)
                }
            }
        }
    }

    fn sequence(&self, sequence: &[Node], at: usize, next: Next<'_>) -> Option<bool> {
        let Some((first, rest)) = sequence.split_first() else {
            return next(at);
        };
        self.node(first, at, &mut |end| self.sequence(rest, end, next))
    }

    fn repeat(
        &self,
        node: &Node,
        min: usize,
        max: Option<usize>,
        count: usize,
        at: usize,
        next: Next<'_>,
    ) -> Option<bool> {
        self.spend()?;
        // Greedy: try one more repetition first, then stop here.
        if max.is_none_or(|max| count < max)
            && self.node(node, at, &mut |end| {
                // A repetition that consumed nothing cannot make progress;
                // stopping it keeps `(a*)*` finite.
                if end == at {
                    return Some(false);
                }
                self.repeat(node, min, max, count + 1, end, next)
            })?
        {
            return Some(true);
        }
        if count >= min { next(at) } else { Some(false) }
    }
}

#[cfg(test)]
mod tests {
    use super::full_match;

    #[test]
    fn validators_match_like_java() {
        // Expected answers are what `"input".matches(pattern)` returns on a JVM.
        let cases: &[(&str, &str, bool)] = &[
            ("[a-z0-9_.-]*", "supermartijn642corelib", true),
            ("[a-z0-9_.-]*", "Bad Id", false),
            ("[a-z0-9_.-]*", "", true),
            ("[a-z0-9_.-]+", "", false),
            ("^[a-z][a-z0-9_]{1,63}$", "carryon", true),
            ("^[a-z][a-z0-9_]{1,63}$", "c", false),
            ("\\d+(\\.\\d+)*", "1.21.10", true),
            ("\\d+(\\.\\d+)*", "1.21.", false),
            ("(?:foo|bar)baz", "barbaz", true),
            ("(?:foo|bar)baz", "foobar", false),
            ("a.c", "abc", true),
            ("a.c", "a\nc", false),
            ("[^\\s]+", "no-space", true),
            ("[^\\s]+", "has space", false),
            ("\\w+:\\w+", "minecraft:stone", true),
            ("x*?y", "xxy", true),
            ("(a*)*b", "aaaaaaaaaaac", false),
            ("[a-]", "-", true),
        ];
        for (pattern, input, expected) in cases {
            assert_eq!(
                full_match(pattern, input),
                Some(*expected),
                "{pattern:?} on {input:?}"
            );
        }
    }

    #[test]
    fn unsupported_syntax_is_not_guessed() {
        for pattern in [
            "\\bword",
            "\\p{L}+",
            "(?i)abc",
            "a++",
            "[a-z&&[^q]]",
            "(a)\\1",
        ] {
            assert_eq!(full_match(pattern, "abc"), None, "{pattern:?}");
        }
    }
}
