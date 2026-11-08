//! A list request's filter: GameSpy's SQL-like condition on a session's
//! fields. DWC writes its own and appends the game's in parentheses
//! (`dwc_match.c`, `DWCi_GetDefaultMatchFilter`; `NetworkLobby.cpp`):
//!
//! ```text
//! dwc_mver = 3 and dwc_pid != 1000000000 and maxplayers = 1 and numplayers < 1
//! and dwc_mtype = 0 and dwc_mresv != dwc_pid and (AP = 463715253 and GT = 0 and CO = 0)
//! ```
//!
//! Comparisons of fields, numbers and quoted texts, joined by `and`, `or` and
//! `not`, with parentheses. Two numbers compare as numbers, anything else as
//! text; a comparison with a field the session lacks is false. An empty filter
//! is every session. A session that fails a filter is told which part it
//! failed and with which values ([`Failed`]), for diagnosing matchmaking.

use std::{
    cmp::Ordering,
    fmt::{self, Display},
};

/// More nesting than this is no filter DWC writes.
const MAX_DEPTH: usize = 16;

#[derive(Debug, PartialEq)]
pub struct Filter(Option<Condition>);

#[derive(Debug, PartialEq)]
enum Condition {
    All(Vec<Condition>),
    Any(Vec<Condition>),
    Not(Box<Condition>),
    Compare(Operand, Op, Operand),
}

#[derive(Debug, PartialEq)]
enum Operand {
    Field(String),
    Value(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Op {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

#[derive(Debug, PartialEq)]
enum Token {
    Word(String),
    Value(String),
    Op(Op),
    Open,
    Close,
}

impl Filter {
    /// The filter `text` writes, or `None` for one it does not understand.
    pub fn parse(text: &str) -> Option<Self> {
        let tokens = tokens(text)?;
        if tokens.is_empty() {
            return Some(Self(None));
        }
        let mut parser = Parser {
            tokens,
            at: 0,
            depth: 0,
        };
        let condition = parser.any()?;
        (parser.at == parser.tokens.len()).then_some(Self(Some(condition)))
    }

    /// Whether a session with these fields passes.
    #[cfg(test)]
    pub fn matches<'f>(&self, field: impl Fn(&str) -> Option<&'f str>) -> bool {
        self.failed(field).is_none()
    }

    /// The part of the filter a session with these fields fails first; none
    /// when it passes.
    pub fn failed<'f>(&self, field: impl Fn(&str) -> Option<&'f str>) -> Option<Failed<'_>> {
        self.0.as_ref()?.failed(&field).map(Failed)
    }
}

/// The part of a filter a session failed: a comparison, or an `or` or `not`
/// as a whole.
#[derive(Debug)]
pub struct Failed<'c>(&'c Condition);

impl Failed<'_> {
    /// The part as the filter writes it, then the session's values of its
    /// fields: `CO = 0 (CO is 1)`.
    pub fn describe<'f>(&self, field: impl Fn(&str) -> Option<&'f str>) -> String {
        let mut names = Vec::new();
        self.0.fields(&mut names);
        let values: Vec<String> = names
            .iter()
            .map(|name| match field(name) {
                Some(value) => format!("{name} is {value}"),
                None => format!("{name} is missing"),
            })
            .collect();
        format!("{} ({})", self.0, values.join(", "))
    }
}

impl Condition {
    /// The first part that fails: the first failing condition of an `and`,
    /// a whole `or` none of whose conditions hold, a `not` whose condition
    /// holds, a comparison that does not hold.
    fn failed<'f>(&self, field: &impl Fn(&str) -> Option<&'f str>) -> Option<&Self> {
        match self {
            Self::All(conditions) => conditions
                .iter()
                .find_map(|condition| condition.failed(field)),
            _ => (!self.holds(field)).then_some(self),
        }
    }

    /// The fields it compares, each once, in the filter's order.
    fn fields<'c>(&'c self, names: &mut Vec<&'c str>) {
        match self {
            Self::All(conditions) | Self::Any(conditions) => {
                for condition in conditions {
                    condition.fields(names);
                }
            }
            Self::Not(condition) => condition.fields(names),
            Self::Compare(left, _, right) => {
                for operand in [left, right] {
                    if let Operand::Field(name) = operand
                        && !names.contains(&name.as_str())
                    {
                        names.push(name);
                    }
                }
            }
        }
    }

    fn holds<'f>(&self, field: &impl Fn(&str) -> Option<&'f str>) -> bool {
        match self {
            Self::All(conditions) => conditions.iter().all(|condition| condition.holds(field)),
            Self::Any(conditions) => conditions.iter().any(|condition| condition.holds(field)),
            Self::Not(condition) => !condition.holds(field),
            Self::Compare(left, op, right) => {
                let (Some(left), Some(right)) = (left.value(field), right.value(field)) else {
                    return false;
                };
                let order = match (left.parse::<i64>(), right.parse::<i64>()) {
                    (Ok(left), Ok(right)) => left.cmp(&right),
                    _ => left.cmp(right),
                };
                op.holds(order)
            }
        }
    }
}

impl Operand {
    fn value<'v, 'f: 'v>(&'v self, field: &impl Fn(&str) -> Option<&'f str>) -> Option<&'v str> {
        match self {
            Self::Field(name) => field(name),
            Self::Value(value) => Some(value),
        }
    }
}

/// As a filter writes it: `or`s inside an `and` and every `not`'s condition
/// in parentheses.
impl Display for Condition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::All(conditions) => {
                for (at, condition) in conditions.iter().enumerate() {
                    if at > 0 {
                        f.write_str(" and ")?;
                    }
                    match condition {
                        Self::Any(_) => write!(f, "({condition})")?,
                        _ => write!(f, "{condition}")?,
                    }
                }
                Ok(())
            }
            Self::Any(conditions) => {
                for (at, condition) in conditions.iter().enumerate() {
                    if at > 0 {
                        f.write_str(" or ")?;
                    }
                    write!(f, "{condition}")?;
                }
                Ok(())
            }
            Self::Not(condition) => write!(f, "not ({condition})"),
            Self::Compare(left, op, right) => write!(f, "{left} {op} {right}"),
        }
    }
}

impl Display for Operand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Field(name) => f.write_str(name),
            Self::Value(value) if value.parse::<i64>().is_ok() => f.write_str(value),
            Self::Value(value) => write!(f, "'{value}'"),
        }
    }
}

impl Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Eq => "=",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Gt => ">",
            Self::Le => "<=",
            Self::Ge => ">=",
        })
    }
}

impl Op {
    fn holds(self, order: Ordering) -> bool {
        match self {
            Self::Eq => order.is_eq(),
            Self::Ne => order.is_ne(),
            Self::Lt => order.is_lt(),
            Self::Gt => order.is_gt(),
            Self::Le => order.is_le(),
            Self::Ge => order.is_ge(),
        }
    }
}

/// `any`: `all` joined by `or`; `all`: `one` joined by `and`; `one`: `not`
/// `one`, a parenthesised `any`, or a comparison.
struct Parser {
    tokens: Vec<Token>,
    at: usize,
    depth: usize,
}

impl Parser {
    fn any(&mut self) -> Option<Condition> {
        let mut conditions = vec![self.all()?];
        while self.keyword("or") {
            conditions.push(self.all()?);
        }
        Some(single_or(conditions, Condition::Any))
    }

    fn all(&mut self) -> Option<Condition> {
        let mut conditions = vec![self.one()?];
        while self.keyword("and") {
            conditions.push(self.one()?);
        }
        Some(single_or(conditions, Condition::All))
    }

    fn one(&mut self) -> Option<Condition> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return None;
        }
        let condition = if self.keyword("not") {
            Condition::Not(Box::new(self.one()?))
        } else if self.next_is(&Token::Open) {
            let condition = self.any()?;
            self.next_is(&Token::Close).then_some(condition)?
        } else {
            let left = self.operand()?;
            let Some(Token::Op(op)) = self.tokens.get(self.at) else {
                return None;
            };
            let op = *op;
            self.at += 1;
            Condition::Compare(left, op, self.operand()?)
        };
        self.depth -= 1;
        Some(condition)
    }

    fn operand(&mut self) -> Option<Operand> {
        let operand = match self.tokens.get(self.at)? {
            Token::Word(word) if !is_keyword(word) => Operand::Field(word.clone()),
            Token::Value(value) => Operand::Value(value.clone()),
            _ => return None,
        };
        self.at += 1;
        Some(operand)
    }

    fn keyword(&mut self, keyword: &str) -> bool {
        let found = matches!(
            self.tokens.get(self.at),
            Some(Token::Word(word)) if word.eq_ignore_ascii_case(keyword)
        );
        self.at += usize::from(found);
        found
    }

    fn next_is(&mut self, token: &Token) -> bool {
        let found = self.tokens.get(self.at) == Some(token);
        self.at += usize::from(found);
        found
    }
}

fn single_or(mut conditions: Vec<Condition>, join: fn(Vec<Condition>) -> Condition) -> Condition {
    if conditions.len() == 1 {
        conditions.remove(0)
    } else {
        join(conditions)
    }
}

fn is_keyword(word: &str) -> bool {
    ["and", "or", "not"]
        .iter()
        .any(|keyword| word.eq_ignore_ascii_case(keyword))
}

/// The filter's words (fields and keywords), values (numbers and quoted
/// texts), operators and parentheses.
fn tokens(text: &str) -> Option<Vec<Token>> {
    let mut tokens = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some(&(start, c)) = chars.peek() {
        chars.next();
        let token = match c {
            c if c.is_whitespace() => continue,
            '(' => Token::Open,
            ')' => Token::Close,
            '\'' | '"' => {
                let end = text[start + 1..].find(c)? + start + 1;
                while chars.next_if(|&(at, _)| at <= end).is_some() {}
                Token::Value(text[start + 1..end].to_owned())
            }
            '=' => {
                chars.next_if(|&(_, next)| next == '=');
                Token::Op(Op::Eq)
            }
            '!' => {
                chars.next_if(|&(_, next)| next == '=')?;
                Token::Op(Op::Ne)
            }
            '<' => Token::Op(
                match chars.next_if(|&(_, next)| next == '=' || next == '>') {
                    Some((_, '=')) => Op::Le,
                    Some(_) => Op::Ne,
                    None => Op::Lt,
                },
            ),
            '>' => Token::Op(if chars.next_if(|&(_, next)| next == '=').is_some() {
                Op::Ge
            } else {
                Op::Gt
            }),
            c if c.is_ascii_digit() || c == '-' => {
                while chars.next_if(|&(_, next)| next.is_ascii_digit()).is_some() {}
                let end = chars.peek().map_or(text.len(), |&(at, _)| at);
                let number = &text[start..end];
                number.parse::<i64>().ok()?;
                Token::Value(number.to_owned())
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                while chars
                    .next_if(|&(_, next)| next.is_ascii_alphanumeric() || next == '_')
                    .is_some()
                {}
                let end = chars.peek().map_or(text.len(), |&(at, _)| at);
                Token::Word(text[start..end].to_owned())
            }
            _ => return None,
        };
        tokens.push(token);
    }
    Some(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Player 1's ranked search, as the server received it.
    const RANKED: &str = "dwc_mver = 3 and dwc_pid != 1000000000 and maxplayers = 1 and numplayers < 1 and dwc_mtype = 0 and dwc_mresv != dwc_pid and (AP = 463715253 and GT = 0 and CO = 0)";
    /// Player 2's heartbeat while searching.
    const SEARCHING: &[(&str, &str)] = &[
        ("numplayers", "0"),
        ("maxplayers", "1"),
        ("dwc_pid", "1000000001"),
        ("dwc_mtype", "0"),
        ("dwc_mresv", "0"),
        ("dwc_mver", "3"),
        ("dwc_eval", "1"),
        ("GT", "0"),
        ("PI", "1000000001"),
        ("AP", "463715253"),
        ("PT", "0"),
        ("CO", "0"),
    ];

    fn passes(filter: &str, fields: &[(&str, &str)]) -> bool {
        Filter::parse(filter)
            .unwrap_or_else(|| panic!("{filter}"))
            .matches(|name| {
                fields
                    .iter()
                    .find(|(field, _)| *field == name)
                    .map(|(_, value)| *value)
            })
    }

    fn with(changes: &[(&str, &str)]) -> Vec<(&'static str, String)> {
        SEARCHING
            .iter()
            .map(|&(field, value)| {
                let changed = changes.iter().find(|(name, _)| *name == field);
                (field, changed.map_or(value, |(_, value)| value).to_owned())
            })
            .collect()
    }

    fn passes_with(filter: &str, changes: &[(&str, &str)]) -> bool {
        let fields = with(changes);
        let fields: Vec<(&str, &str)> = fields
            .iter()
            .map(|(field, value)| (*field, value.as_str()))
            .collect();
        passes(filter, &fields)
    }

    #[test]
    fn a_ranked_search_finds_another_searching_game() {
        assert!(passes(RANKED, SEARCHING));
    }

    #[test]
    fn a_ranked_search_skips_itself_and_games_that_do_not_fit() {
        for changes in [
            &[("dwc_pid", "1000000000")][..],
            &[("numplayers", "1")],
            &[("maxplayers", "2")],
            &[("dwc_mtype", "2")],
            &[("dwc_mresv", "1000000001")],
            &[("AP", "1")],
            &[("GT", "1")],
            &[("CO", "1")],
        ] {
            assert!(!passes_with(RANKED, changes), "{changes:?}");
        }
    }

    /// Why the session with `changes` fails `filter`; none when it passes.
    fn failed_with(filter: &str, changes: &[(&str, &str)]) -> Option<String> {
        let fields = with(changes);
        let field = |name: &str| {
            fields
                .iter()
                .find(|(field, _)| *field == name)
                .map(|(_, value)| value.as_str())
        };
        let filter = Filter::parse(filter).unwrap();
        filter.failed(field).map(|failed| failed.describe(field))
    }

    #[test]
    fn a_failed_search_names_the_part_and_the_values() {
        assert_eq!(failed_with(RANKED, &[]), None);
        assert_eq!(
            failed_with(RANKED, &[("CO", "1")]).as_deref(),
            Some("CO = 0 (CO is 1)")
        );
        // The first part that fails, inside the game's parentheses too.
        assert_eq!(
            failed_with(RANKED, &[("AP", "467909557"), ("GT", "2")]).as_deref(),
            Some("AP = 463715253 (AP is 467909557)")
        );
        assert_eq!(
            failed_with(RANKED, &[("dwc_mresv", "1000000001")]).as_deref(),
            Some("dwc_mresv != dwc_pid (dwc_mresv is 1000000001, dwc_pid is 1000000001)")
        );
        let turned_down = RANKED.replace("CO = 0)", "CO = 0 and PI != 1000000001)");
        assert_eq!(
            failed_with(&turned_down, &[]).as_deref(),
            Some("PI != 1000000001 (PI is 1000000001)")
        );
        assert_eq!(
            failed_with("missing = 'x' or not (GT = 0)", &[]).as_deref(),
            Some("missing = 'x' or not (GT = 0) (missing is missing, GT is 0)")
        );
        assert_eq!(
            failed_with("CO = 1 and (GT = 1 or GT = 2)", &[("CO", "1")]).as_deref(),
            Some("GT = 1 or GT = 2 (GT is 0)")
        );
    }

    #[test]
    fn filters_write_back_as_they_read() {
        for filter in [
            "a = 1 and (b = 2 or c < 3) and not (d >= 'x')",
            "a != b or c <= -4",
        ] {
            let parsed = Filter::parse(filter).unwrap();
            assert_eq!(parsed.0.unwrap().to_string(), filter);
        }
    }

    #[test]
    fn and_binds_closer_than_or() {
        let fields = [("a", "1"), ("b", "0"), ("c", "0")];
        assert!(passes("a = 1 or b = 1 and c = 1", &fields));
        assert!(!passes("(a = 1 or b = 1) and c = 1", &fields));
        assert!(passes("not (b = 1) and NOT c = 1", &fields));
        assert!(!passes("not a = 1", &fields));
    }

    #[test]
    fn numbers_compare_as_numbers_and_the_rest_as_text() {
        let fields = [("n", "9"), ("m", "-3"), ("s", "openplaying")];
        assert!(passes("n < 10", &fields));
        assert!(passes("n = 09 and n >= 9 and n <= 9 and n <> 8", &fields));
        assert!(passes("m < 0 and m > -4 and m == -3", &fields));
        assert!(passes("s = 'openplaying' and s != \"closed\"", &fields));
        assert!(passes("n != m and s > n", &fields));
    }

    #[test]
    fn a_missing_field_passes_no_comparison() {
        assert!(!passes("missing = 1", SEARCHING));
        assert!(!passes("missing != 1", SEARCHING));
        assert!(passes("not missing = 1", SEARCHING));
    }

    #[test]
    fn an_empty_filter_passes_every_session() {
        assert!(passes("", &[]));
        assert!(passes("  ", SEARCHING));
    }

    #[test]
    fn filters_it_does_not_understand_are_none() {
        for filter in [
            "dwc_pid =",
            "= 1",
            "(a = 1",
            "a = 1)",
            "a = 1 and",
            "a ~ 1",
            "a ! 1",
            "a = 'open",
            "a = 1 b = 2",
            "a like 'x'",
            "and = 1",
            "a = 99999999999999999999",
        ] {
            assert_eq!(Filter::parse(filter), None, "{filter}");
        }
        let deep = format!("{}a = 1{}", "(".repeat(MAX_DEPTH), ")".repeat(MAX_DEPTH));
        assert_eq!(Filter::parse(&deep), None);
        let fits = format!(
            "{}a = 1{}",
            "(".repeat(MAX_DEPTH - 1),
            ")".repeat(MAX_DEPTH - 1)
        );
        assert!(passes(&fits, &[("a", "1")]));
    }
}
