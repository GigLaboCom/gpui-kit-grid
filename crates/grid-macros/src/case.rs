//! The naming rules of `#[grid_columns(rename_all = "…")]`.

/// How a variant name becomes a column id when `#[grid_column(id = "…")]`
/// does not give one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum RenameRule {
    /// `BidSize` → `bid_size`: the id every hand-written implementation in
    /// the repository uses.
    #[default]
    SnakeCase,
    /// `BidSize` → `bid-size`.
    KebabCase,
    /// `BidSize` → `bidSize`.
    CamelCase,
    /// `BidSize` → `BidSize`: the variant name as written.
    PascalCase,
    /// `BidSize` → `bidsize`.
    Lowercase,
}

impl RenameRule {
    /// Every rule with its name in the attribute, in the order the error
    /// for an unknown rule lists them.
    pub(crate) const ALL: [(&'static str, Self); 5] = [
        ("snake_case", Self::SnakeCase),
        ("kebab-case", Self::KebabCase),
        ("camelCase", Self::CamelCase),
        ("PascalCase", Self::PascalCase),
        ("lowercase", Self::Lowercase),
    ];

    /// The rule named `name` in the attribute.
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .find(|(rule_name, _)| *rule_name == name)
            .map(|(_, rule)| *rule)
    }

    /// The column id of a variant named `name`.
    pub(crate) fn apply(self, name: &str) -> String {
        match self {
            Self::SnakeCase => join_lowercase(name, "_"),
            Self::KebabCase => join_lowercase(name, "-"),
            Self::CamelCase => {
                let mut id = String::with_capacity(name.len());
                for (ix, word) in words(name).into_iter().enumerate() {
                    if ix == 0 {
                        id.extend(word.chars().flat_map(char::to_lowercase));
                    } else {
                        let mut chars = word.chars();
                        id.extend(chars.next().into_iter().flat_map(char::to_uppercase));
                        id.extend(chars.flat_map(char::to_lowercase));
                    }
                }
                id
            }
            Self::PascalCase => name.to_owned(),
            Self::Lowercase => name.to_lowercase(),
        }
    }
}

fn join_lowercase(name: &str, separator: &str) -> String {
    let mut id = String::with_capacity(name.len() + 4);
    for (ix, word) in words(name).into_iter().enumerate() {
        if ix > 0 {
            id.push_str(separator);
        }
        id.extend(word.chars().flat_map(char::to_lowercase));
    }
    id
}

/// The case of the last cased character of the current word.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WordCase {
    /// No cased character since the word began.
    None,
    Lower,
    Upper,
}

/// Splits a name into words as the `heck` crate does: at characters that
/// are not alphanumeric (`_`), before an uppercase letter that follows a
/// lowercase one (a digit keeps the case before it: `Leg2Price` → `Leg2`,
/// `Price`), and before the last letter of an uppercase run that a lowercase
/// letter follows (`HTTPStatus` → `HTTP`, `Status`).
fn words(name: &str) -> Vec<&str> {
    let mut words = Vec::new();
    for part in name.split(|c: char| !c.is_alphanumeric()) {
        let mut start = 0;
        let mut case = WordCase::None;
        let mut chars = part.char_indices().peekable();
        while let Some((ix, c)) = chars.next() {
            let Some(&(next_ix, next)) = chars.peek() else {
                words.push(&part[start..]);
                break;
            };
            let case_with_c = if c.is_lowercase() {
                WordCase::Lower
            } else if c.is_uppercase() {
                WordCase::Upper
            } else {
                case
            };
            if case_with_c == WordCase::Lower && next.is_uppercase() {
                words.push(&part[start..next_ix]);
                start = next_ix;
                case = WordCase::None;
            } else if case == WordCase::Upper && c.is_uppercase() && next.is_lowercase() {
                words.push(&part[start..ix]);
                start = ix;
                case = WordCase::None;
            } else {
                case = case_with_c;
            }
        }
    }
    words
}

#[cfg(test)]
mod tests {
    use super::RenameRule;

    #[test]
    fn test_case_rules() {
        // Name, then snake_case, kebab-case, camelCase, PascalCase, lowercase.
        let cases = [
            ("Id", ["id", "id", "id", "Id", "id"]),
            (
                "BidSize",
                ["bid_size", "bid-size", "bidSize", "BidSize", "bidsize"],
            ),
            (
                "HTTPStatus",
                [
                    "http_status",
                    "http-status",
                    "httpStatus",
                    "HTTPStatus",
                    "httpstatus",
                ],
            ),
            (
                "Leg2Price",
                [
                    "leg2_price",
                    "leg2-price",
                    "leg2Price",
                    "Leg2Price",
                    "leg2price",
                ],
            ),
            ("A", ["a", "a", "a", "A", "a"]),
            ("ABC", ["abc", "abc", "abc", "ABC", "abc"]),
            (
                "Bid_Size",
                ["bid_size", "bid-size", "bidSize", "Bid_Size", "bid_size"],
            ),
        ];
        for (name, expected) in cases {
            for ((rule_name, rule), id) in RenameRule::ALL.into_iter().zip(expected) {
                assert_eq!(rule.apply(name), id, "{name} by {rule_name}");
            }
        }
        assert_eq!(
            RenameRule::from_name("kebab-case"),
            Some(RenameRule::KebabCase)
        );
        assert_eq!(RenameRule::from_name("SCREAMING_SNAKE_CASE"), None);
        assert_eq!(RenameRule::default(), RenameRule::SnakeCase);
    }
}
