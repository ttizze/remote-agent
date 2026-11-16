//! Search decisions shared by the canonical Host and its local-list consumer.
//! Matches the supported fields in T3 1a12c4af's paletteSearch.ts. Bex returns
//! conversation rows, so highlight/snippet caches and palette picks have no role.
use regex::Regex;
use std::sync::LazyLock;

static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[\p{L}\p{N}]$").unwrap());
static LETTER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\p{L}$").unwrap());
static SYMBOL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\p{So}$").unwrap());
static MARK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\p{M}$").unwrap());
static LINKS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"!?\[([^\]\n]*)\]\([^)\s]*\)").unwrap());

fn property(expression: &Regex, character: char) -> bool {
    expression.is_match(character.encode_utf8(&mut [0; 4]))
}
static LOWER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\p{Ll}$").unwrap());
static UPPER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\p{Lu}$").unwrap());
fn alphanumeric(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_alphanumeric()
    } else {
        property(&WORD, c)
    }
}
fn letter(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_alphabetic()
    } else {
        property(&LETTER, c)
    }
}
fn symbol(c: char) -> bool {
    !c.is_ascii() && property(&SYMBOL, c)
}
fn mark(c: char) -> bool {
    !c.is_ascii() && property(&MARK, c)
}
fn lowercase(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_lowercase()
    } else {
        property(&LOWER, c)
    }
}
fn uppercase(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_uppercase()
    } else {
        property(&UPPER, c)
    }
}
fn character(value: u16) -> char {
    char::from_u32(value.into()).unwrap_or('\0')
}
fn word(value: u16) -> bool {
    alphanumeric(character(value))
}
fn cjk(c: char) -> bool {
    matches!(c as u32, 0x3040..=0x30ff | 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff)
}
fn whitespace(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | ' ' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}
fn wrapping(c: char) -> bool {
    whitespace(c) || matches!(c, '"' | '\'' | '`' | '“' | '”' | '‘' | '’')
}
fn filler(token: &str) -> bool {
    matches!(
        token,
        "a" | "an" | "and" | "for" | "in" | "of" | "on" | "the" | "to" | "with"
    )
}

// Keep UTF-16 length, as the reference does. Expanding ligatures would change
// word coordinates and make unrelated spelling match (ﬁ is not the two letters fi).
fn fold(text: &str) -> String {
    if text.is_ascii() {
        return text.to_ascii_lowercase();
    }
    let normalizer = icu_normalizer::DecomposingNormalizerBorrowed::new_nfkd();
    let mut result = String::with_capacity(text.len());
    for c in text.chars() {
        if !c.is_ascii() && c.len_utf16() == 1 {
            let mut normalized = normalizer
                .normalize_iter(std::iter::once(c))
                .filter(|&c| !mark(c))
                .flat_map(char::to_lowercase)
                .map(|c| match c {
                    'ς' => 'σ',
                    '‘' | '’' => '\'',
                    _ => c,
                });
            if let Some(first) = normalized.next().filter(|c| c.len_utf16() == 1)
                && normalized.next().is_none()
            {
                result.push(first);
                continue;
            }
        }
        result.push(c);
    }
    result.to_lowercase()
}
fn nfc(text: &str) -> String {
    icu_normalizer::ComposingNormalizerBorrowed::new_nfc()
        .normalize(text)
        .into_owned()
}
fn starts_word(before: char, previous: char, current: char, next: char) -> bool {
    let after_apostrophe = matches!(previous, '\'' | '’') && alphanumeric(before);
    (alphanumeric(current) || symbol(current))
        && !after_apostrophe
        && (previous == '\0'
            || !alphanumeric(previous)
            || lowercase(previous) && uppercase(current)
            || uppercase(previous) && uppercase(current) && lowercase(next)
            || previous.is_numeric() != current.is_numeric()
            || cjk(current))
}
fn unit(text: &[u16], index: usize) -> char {
    text.get(index).copied().map(character).unwrap_or('\0')
}
fn starts_at(text: &[u16], index: usize) -> bool {
    let current = unit(text, index);
    if !alphanumeric(current) && !symbol(current) {
        return true;
    }
    starts_word(
        index.checked_sub(2).map_or('\0', |i| unit(text, i)),
        index.checked_sub(1).map_or('\0', |i| unit(text, i)),
        current,
        unit(text, index + 1),
    )
}
fn ends_at(text: &[u16], index: usize) -> bool {
    index >= text.len() || !word(text[index]) || starts_at(text, index)
}

struct Field {
    text: Vec<u16>,
    starts: Vec<bool>,
}
impl Field {
    fn new(text: &str) -> Self {
        let chars: Vec<_> = text.chars().collect();
        let mut starts = vec![false; text.encode_utf16().count()];
        let mut at = 0;
        for (i, &c) in chars.iter().enumerate() {
            starts[at] = starts_word(
                i.checked_sub(2).map_or('\0', |i| chars[i]),
                i.checked_sub(1).map_or('\0', |i| chars[i]),
                c,
                chars.get(i + 1).copied().unwrap_or('\0'),
            );
            at += c.len_utf16();
        }
        Self {
            text: fold(text).encode_utf16().collect(),
            starts,
        }
    }
    fn word_end(&self, start: usize) -> usize {
        let mut end = start + 1;
        while end < self.text.len() && !self.starts[end] && word(self.text[end]) {
            end += 1;
        }
        end
    }
}
fn occurrences<'a>(text: &'a [u16], token: &'a [u16]) -> impl Iterator<Item = usize> + 'a {
    text.windows(token.len().max(1))
        .enumerate()
        .filter_map(move |(i, part)| (!token.is_empty() && part == token).then_some(i))
}
fn within_one_edit(left: &[u16], right: &[u16], swap_only: bool) -> bool {
    if left.len().abs_diff(right.len()) > usize::from(!swap_only) {
        return false;
    }
    let same = left.iter().zip(right).take_while(|(a, b)| a == b).count();
    let a = &left[same..];
    let b = &right[same..];
    let swapped = a.len() > 1 && b.len() > 1 && a[0] == b[1] && a[1] == b[0] && a[2..] == b[2..];
    if swap_only {
        return swapped;
    }
    swapped || a == b || a.get(1..) == b.get(1..) || a.get(1..) == Some(b) || Some(a) == b.get(1..)
}
fn joined(field: &Field, token: &[u16]) -> bool {
    for start in occurrences(&field.text, &token[..1]).filter(|&i| field.starts[i]) {
        let mut runs = Vec::new();
        let mut at = start;
        let mut used = 0;
        while used < token.len() && at < field.text.len() {
            let end = field.word_end(at);
            let rest = &token[used..];
            if rest.len() <= end - at && field.text[at..].starts_with(rest) {
                runs.push((at, at + rest.len()));
                used = token.len();
            } else if rest.starts_with(&field.text[at..end]) {
                runs.push((at, end));
                used += end - at;
                at = end;
                while at < field.text.len() && !field.starts[at] {
                    at += 1;
                }
            } else {
                break;
            }
        }
        let Some(&(last_start, last_end)) = runs.last() else {
            continue;
        };
        if used == token.len()
            && runs.len() >= 2
            && (last_end == field.word_end(last_start)
                || last_end - last_start >= 3
                || Some(start) == field.starts.iter().position(|&s| s)
                || runs[..runs.len() - 1].iter().all(|(s, e)| e - s >= 3))
        {
            return true;
        }
    }
    false
}
fn typo(field: &Field, token: &[u16]) -> bool {
    if token.len() < 3 || !token.iter().any(|&c| letter(character(c))) {
        return false;
    }
    let short = token.len() <= 4;
    for index in (0..field.text.len()).filter(|&i| field.starts[i]) {
        let first_kept = field.text[index] == token[0]
            || token.len() > 3
                && field.text[index] == token[1]
                && field.text.get(index + 1) == Some(&token[0]);
        if !first_kept {
            continue;
        }
        let mut end = index + 1;
        while end < field.text.len() && word(field.text[end]) {
            end += 1;
        }
        let lengths = if short {
            vec![token.len(), token.len() + 1]
        } else {
            vec![token.len(), token.len() - 1, token.len() + 1]
        };
        if lengths.into_iter().any(|length| {
            length <= end - index
                && within_one_edit(
                    token,
                    &field.text[index..index + length],
                    short && length == token.len(),
                )
        }) {
            return true;
        }
    }
    false
}
fn token_score(
    field: &Field,
    token: &[u16],
    typos: bool,
    primary: bool,
    mid_word: bool,
) -> Option<f64> {
    if token.is_empty() {
        return None;
    }
    let mut best: Option<f64> = None;
    let own_boundary = !word(token[0]) && !symbol(character(token[0]));
    for at in occurrences(&field.text, token) {
        if field.starts[at] || own_boundary {
            let end = at + token.len();
            let whole = end >= field.text.len() || field.starts[end] || !word(field.text[end]);
            let mut run_end = end;
            while run_end < field.text.len() && word(field.text[run_end]) {
                run_end += 1;
            }
            let score = if whole {
                1.0
            } else {
                0.7 + 0.2 * token.len() as f64 / (run_end - at) as f64
            };
            best = Some(best.unwrap_or(0.0).max(score));
            if whole {
                return best;
            }
        } else if mid_word && token.len() >= 4 && best.is_none() {
            best = Some(0.3);
        }
    }
    if best.is_none_or(|score| score <= 0.3) && primary && joined(field, token) {
        return Some(0.6);
    }
    if best.is_some_and(|score| score > 0.3) || !typos {
        return best;
    }
    if typo(field, token) { Some(0.45) } else { best }
}
fn strip_branch_prefix(text: &str) -> &str {
    for prefix in ["t3/", "t3code/"] {
        if let Some(rest) = text
            .strip_prefix(prefix)
            .filter(|rest| rest.chars().next().is_some_and(|c| !whitespace(c)))
        {
            return rest;
        }
    }
    if let Some(rest) = text.strip_prefix("t3-").filter(|rest| {
        rest.len() == 8
            && rest
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    }) {
        return rest;
    }
    text
}
fn field_query(query: &str) -> String {
    let mut query = query;
    for prefix in ["pull request", "pull", "pr"] {
        if let Some(rest) = query.strip_prefix(prefix) {
            let rest = rest.trim_start_matches(whitespace);
            if rest
                .trim_start_matches('#')
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
            {
                query = rest;
                break;
            }
        }
    }
    query = query
        .strip_prefix('#')
        .unwrap_or(query)
        .trim_start_matches(whitespace);
    strip_branch_prefix(query)
        .split(' ')
        .map(|word| {
            word.strip_prefix("t3-")
                .filter(|rest| {
                    !rest.is_empty()
                        && rest.len() <= 8
                        && rest.bytes().all(|b| b.is_ascii_hexdigit())
                })
                .unwrap_or(word)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

struct WeightedField {
    field: Field,
    weight: f64,
    context: bool,
    mid_word: bool,
}
struct FieldMatch {
    quality: f64,
    strong: bool,
    whole_words: bool,
}
fn score_fields(query: &str, fields: &[WeightedField], typos: bool) -> Option<FieldMatch> {
    let tokens: Vec<_> = query.split(' ').filter(|t| !t.is_empty()).collect();
    if tokens.is_empty() {
        return None;
    }
    let optional = tokens.iter().any(|token| !filler(token));
    let mut total = 0.0;
    let mut count = 0;
    let mut whole = true;
    let mut strong = true;
    for token in &tokens {
        let units: Vec<_> = token.encode_utf16().collect();
        let mut best = 0.0_f64;
        let mut start = false;
        for (i, weighted) in fields.iter().enumerate() {
            if let Some(score) =
                token_score(&weighted.field, &units, typos, i == 0, weighted.mid_word)
            {
                best = best.max(score * weighted.weight);
                start |= score >= 0.7 && !weighted.context;
            }
        }
        if best == 0.0 {
            if optional && filler(token) {
                continue;
            }
            return None;
        }
        strong &= start;
        total += best;
        count += 1;
        whole &= if filler(token) {
            optional
        } else {
            units.len() >= 3 && best >= 1.0
        };
    }
    if count == 0 {
        return None;
    }
    let full: Vec<_> = query.encode_utf16().collect();
    let mut quality = total / count as f64;
    if fields[0].field.text == full {
        quality += 0.5;
    } else if tokens.len() > 1 && occurrences(&fields[0].field.text, &full).next().is_some() {
        quality += 0.25;
    }
    Some(FieldMatch {
        quality,
        strong,
        whole_words: whole,
    })
}

/// Bex keeps unregistered folders usable. Assigned projects must still exist;
/// neither deleted/archived threads nor subagents/side chats are search rows.
pub fn visible_thread(
    relationship: Option<&str>,
    side_chat: Option<bool>,
    deleted: bool,
    archived: bool,
    project_assigned: bool,
    project_exists: bool,
) -> bool {
    !deleted
        && !archived
        && crate::orchestration::shell::is_listed_lineage(relationship, side_chat)
        && (!project_assigned || project_exists)
}

pub const ATTACHMENT_ONLY_BOOTSTRAP_PROMPT: &str = "[User attached one or more files without additional text. Respond using the conversation context and the attached files.]";
fn long_message_token(token: &str) -> bool {
    let length = token.chars().count();
    length >= 3
        || length == 2
            && token
                .chars()
                .any(|c| cjk(c) || matches!(c as u32, 0xac00..=0xd7af))
}
/// SQLite lower() folds ASCII only. A short non-ASCII narrowing word needs
/// the reference's case/NFD spellings; the full matcher still grades the hit.
pub fn short_token_spellings(token: &str) -> Vec<String> {
    let nfd = icu_normalizer::DecomposingNormalizerBorrowed::new_nfd();
    let mut spellings = vec![String::new()];
    for c in token.chars() {
        let plain: String = nfd
            .normalize_iter(std::iter::once(c))
            .filter(|&c| !mark(c))
            .collect();
        let mut forms = vec![
            c.to_string(),
            c.to_uppercase().collect(),
            plain.to_uppercase(),
            plain,
        ];
        forms.sort();
        forms.dedup();
        spellings = spellings
            .into_iter()
            .flat_map(|prefix| forms.iter().map(move |form| format!("{prefix}{form}")))
            .collect();
    }
    spellings.sort();
    spellings.dedup();
    spellings
}

/// Values a search actually ranks; no mutable thread or client state is needed.
pub struct TitleSubject<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub branch: Option<&'a str>,
    pub project: Option<&'a str>,
    pub activity_at_ms: Option<i64>,
}
struct TitleMatch {
    quality: f64,
    strong: bool,
    whole_words: bool,
    score: f64,
}

/// T3 ranks activity by the latest authored input, or the thread's creation.
/// Metadata/status updates do not make an old conversation a recent match.
pub fn authored_activity_ms(latest_authored: Option<&str>, created_at: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(latest_authored.unwrap_or(created_at))
        .ok()
        .map(|at| at.timestamp_millis())
}

/// Recency of authored activity; Bex's sidebar has no palette pick/open score.
pub fn usage_boost(activity: Option<i64>, now: i64) -> f64 {
    1.0 + activity.map_or(0.0, |at| {
        2.0_f64.powf(-(now.saturating_sub(at).max(0) as f64) / (14.0 * 24.0 * 60.0 * 60.0 * 1000.0))
    })
}
fn match_class(quality: f64) -> u8 {
    if quality >= 1.5 {
        3
    } else if quality >= 0.7 {
        2
    } else if quality >= 0.4 {
        1
    } else {
        0
    }
}

pub struct Query {
    normalized: String,
    field: String,
    words: Vec<String>,
}
impl Query {
    pub fn new(query: &str) -> Self {
        let normalized = fold(&nfc(query.trim_matches(wrapping)))
            .split(whitespace)
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let field = field_query(&normalized);
        let words = nfc(&query.trim_matches(wrapping).replace(['‘', '’'], "'"))
            .to_lowercase()
            .split(whitespace)
            .filter(|word| !word.is_empty())
            .map(str::to_owned)
            .collect();
        Self {
            normalized,
            field,
            words,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.normalized.is_empty()
    }
    fn title(&self, subject: &TitleSubject<'_>, typos: bool, now: i64) -> Option<TitleMatch> {
        let fields = [
            WeightedField {
                field: Field::new(subject.title),
                weight: 1.0,
                context: false,
                mid_word: true,
            },
            WeightedField {
                field: Field::new(strip_branch_prefix(subject.branch.unwrap_or_default())),
                weight: 0.6,
                context: false,
                mid_word: false,
            },
            WeightedField {
                field: Field::new(subject.project.unwrap_or_default()),
                weight: 0.25,
                context: true,
                mid_word: false,
            },
        ];
        let Some(matched) = score_fields(&self.field, &fields, typos) else {
            return self.reference(subject, now);
        };
        Some(TitleMatch {
            quality: matched.quality,
            strong: matched.strong,
            whole_words: matched.whole_words,
            score: matched.quality * usage_boost(subject.activity_at_ms, now),
        })
    }
    fn reference(&self, subject: &TitleSubject<'_>, now: i64) -> Option<TitleMatch> {
        if subject.id.is_empty() || self.normalized.encode_utf16().count() < 8 {
            return None;
        }
        let id = subject.id.to_lowercase();
        let at = id.find(&self.normalized);
        let not_cut = |value: &str, at: usize| {
            !value
                .get(at..)
                .and_then(|s| s.chars().next())
                .is_some_and(char::is_numeric)
        };
        let matches = if self.normalized.chars().any(char::is_numeric) {
            at.is_some_and(|i| not_cut(&id, i + self.normalized.len()))
                || self.normalized.starts_with(&id) && not_cut(&self.normalized, id.len())
        } else {
            at == Some(0)
        };
        matches.then(|| TitleMatch {
            quality: 0.6,
            strong: true,
            whole_words: false,
            score: 0.6 * usage_boost(subject.activity_at_ms, now),
        })
    }
    pub fn rank_titles(&self, subjects: &[TitleSubject<'_>], now: i64) -> Vec<usize> {
        let rank = |typos| {
            subjects
                .iter()
                .enumerate()
                .filter_map(|(i, subject)| self.title(subject, typos, now).map(|m| (i, m)))
                .collect::<Vec<_>>()
        };
        let mut ranked = rank(false);
        if !ranked.iter().any(|(_, m)| m.strong) {
            ranked = rank(true);
        }
        ranked.sort_by(|(_, a), (_, b)| {
            match_class(b.quality)
                .min(2)
                .cmp(&match_class(a.quality).min(2))
                .then_with(|| match_class(b.quality).cmp(&match_class(a.quality)))
                .then_with(|| b.whole_words.cmp(&a.whole_words))
                .then_with(|| b.score.total_cmp(&a.score))
        });
        ranked.into_iter().map(|(i, _)| i).collect()
    }
    pub fn matches_project(&self, title: &str) -> bool {
        score_fields(
            &self.normalized,
            &[WeightedField {
                field: Field::new(title),
                weight: 1.0,
                context: false,
                mid_word: false,
            }],
            false,
        )
        .is_some()
    }
    /// Search terms and shorter narrowing words, in their typed order.
    pub fn message_tokens(&self) -> (Vec<&str>, Vec<&str>) {
        self.words
            .iter()
            .map(String::as_str)
            .partition(|token| long_message_token(token))
    }
    /// The provider's rendered link label is searchable; its hidden URL is not.
    pub fn message(&self, text: &str) -> Option<f64> {
        let ordinary = text
            .get(..11)
            .filter(|prefix| prefix.eq_ignore_ascii_case("Ultrathink:"))
            .map_or(text, |_| text[11..].trim_start_matches(whitespace));
        if ordinary == ATTACHMENT_ONLY_BOOTSTRAP_PROMPT {
            return None;
        }
        if !self.words.iter().any(|token| long_message_token(token)) {
            return None;
        }
        let text = LINKS.replace_all(text, "$1");
        let original: Vec<_> = text.encode_utf16().collect();
        let folded: Vec<_> = fold(&text).encode_utf16().collect();
        let lower: Vec<_> = text.to_lowercase().encode_utf16().collect();
        let plain = if lower.len() == original.len() {
            &lower
        } else {
            &folded
        };
        let mut quality = 1.0_f64;
        let mut positions = Vec::new();
        let mut count = 0;
        for token in &self.words {
            let units: Vec<_> = token.encode_utf16().collect();
            let folded_token: Vec<_> = fold(token)
                .chars()
                .filter(|&c| !mark(c))
                .collect::<String>()
                .encode_utf16()
                .collect();
            let mut found = message_token(&original, plain, &units, self.words.len() == 1);
            if found.as_ref().is_none_or(|m| m.0 != 1.0)
                && !folded_token.is_empty()
                && (!token.is_ascii() || !text.is_ascii())
            {
                found = message_token(&original, &folded, &folded_token, self.words.len() == 1);
            }
            let (score, at) = found?;
            if score == 0.3 && token.chars().count() < 4 {
                return None;
            }
            if long_message_token(token) {
                quality = quality.min(score);
                count += 1;
            }
            positions.push(at);
        }
        if self.words.len() > 1 {
            let words: Vec<Vec<u16>> = self
                .words
                .iter()
                .map(|word| word.encode_utf16().collect())
                .collect();
            let plain: Vec<_> = plain
                .iter()
                .map(|&u| if matches!(u, 0x2018 | 0x2019) { 39 } else { u })
                .collect();
            let folded_words: Vec<Vec<u16>> = self
                .words
                .iter()
                .map(|word| {
                    fold(word)
                        .chars()
                        .filter(|&c| !mark(c))
                        .collect::<String>()
                        .encode_utf16()
                        .collect()
                })
                .collect();
            let folded: Vec<_> = folded
                .iter()
                .map(|&u| if matches!(u, 0x2018 | 0x2019) { 39 } else { u })
                .collect();
            if phrase(&plain, &words)
                || (!text.is_ascii() || !self.normalized.is_ascii())
                    && phrase(&folded, &folded_words)
            {
                return Some(quality + 0.25);
            }
        }
        Some(
            quality
                + if count > 1 && shortest_span(&positions) <= 60 {
                    0.1
                } else {
                    0.0
                },
        )
    }
}
fn message_token(
    original: &[u16],
    folded: &[u16],
    token: &[u16],
    first_only: bool,
) -> Option<(f64, Vec<usize>)> {
    let mut quality = 0.0;
    let mut places = Vec::new();
    let mut run = (0, 0, false);
    for i in occurrences(folded, token) {
        if i + token.len() > run.1 {
            let mut start = i;
            while start > 0 && word(original[start - 1]) {
                start -= 1;
            }
            let mut end = i + token.len();
            while end < original.len() && word(original[end]) {
                end += 1;
            }
            let text = &original[start..end];
            let hex = text.len() >= 7
                && text
                    .iter()
                    .all(|&c| c <= 127 && (c as u8).is_ascii_hexdigit())
                && text.iter().any(|&c| c <= 127 && (c as u8).is_ascii_digit());
            run = (start, end, hex);
        }
        if run.2 && run.0 < i {
            continue;
        }
        let found = if !starts_at(original, i) {
            0.3
        } else if ends_at(original, i + token.len()) {
            1.0
        } else {
            0.8
        };
        if found > quality {
            quality = found;
            places = vec![i];
            if first_only && quality == 1.0 {
                break;
            }
        } else if !first_only && found == quality {
            places.push(i);
        }
    }
    (!places.is_empty()).then_some((quality, places))
}
fn phrase(text: &[u16], words: &[Vec<u16>]) -> bool {
    for start in occurrences(text, &words[0]) {
        if start > 0 && word(text[start - 1]) {
            continue;
        }
        let mut at = start + words[0].len();
        let mut matched = true;
        for token in &words[1..] {
            let before = at;
            while at < text.len() && whitespace(character(text[at])) {
                at += 1;
            }
            if at == before || !text[at..].starts_with(token) {
                matched = false;
                break;
            }
            at += token.len();
        }
        if matched && (at == text.len() || !word(text[at])) {
            return true;
        }
    }
    false
}
fn shortest_span(positions: &[Vec<usize>]) -> usize {
    let mut indexes = vec![0; positions.len()];
    let mut best = usize::MAX;
    loop {
        let Some((first, &low)) = positions
            .iter()
            .zip(&indexes)
            .enumerate()
            .map(|(i, (p, &at))| (i, &p[at]))
            .min_by_key(|(_, at)| **at)
        else {
            return 0;
        };
        let high = positions
            .iter()
            .zip(&indexes)
            .map(|(p, &at)| p[at])
            .max()
            .unwrap();
        best = best.min(high - low);
        indexes[first] += 1;
        if indexes[first] == positions[first].len() {
            return best;
        }
    }
}
