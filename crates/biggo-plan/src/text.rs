//! Text functions on single values. The engine and the virtual machine both compute with
//! these, so a function gives the same answer on a column as on a value.

use regex::Regex;

/// The longest string that padding may produce.
pub const MAX_PADDED: i64 = 1_000_000;

/// The byte offset of the character at `index`, or the length of `text` if it is shorter.
fn offset(text: &str, index: usize) -> usize {
    match text.char_indices().nth(index) {
        Some((at, _)) => at,
        None => text.len(),
    }
}

/// The part of `text` that starts at character `start` and is `length` characters long, or
/// runs to the end. A negative `start` counts from the end. A position outside the text is
/// brought to its nearest end, so the result is shorter rather than an error.
pub fn substring(text: &str, start: i64, length: Option<i64>) -> &str {
    let chars = text.chars().count() as i64;
    let start = match start < 0 {
        true => chars.saturating_add(start).max(0),
        false => start.min(chars),
    };
    let end = match length {
        Some(length) => start.saturating_add(length.max(0)).min(chars),
        None => chars,
    };
    let from = offset(text, start as usize);
    let to = from + offset(&text[from..], (end - start) as usize);
    &text[from..to]
}

/// `text` with every `from` replaced by `to`. An empty `from` matches nothing.
pub fn replace(text: &str, from: &str, to: &str) -> String {
    match from.is_empty() {
        true => text.to_string(),
        false => text.replace(from, to),
    }
}

/// The pieces of `text` between its separators. An empty separator splits nothing.
pub fn split<'t>(text: &'t str, separator: &str) -> Vec<&'t str> {
    match separator.is_empty() {
        true => vec![text],
        false => text.split(separator).collect(),
    }
}

/// The piece of `text` at `index`, counted from 0, or from the last piece when negative.
/// `None` if there are not that many pieces.
pub fn split_part<'t>(text: &'t str, separator: &str, index: i64) -> Option<&'t str> {
    if separator.is_empty() {
        return matches!(index, 0 | -1).then_some(text);
    }
    match index < 0 {
        true => text
            .rsplit(separator)
            .nth(usize::try_from(-(index + 1)).ok()?),
        false => text.split(separator).nth(usize::try_from(index).ok()?),
    }
}

/// `text` made `width` characters long by adding `fill` before or after it, repeated and cut
/// as needed. A text that is already that long is returned as it is, and so is any text when
/// `fill` is empty.
pub fn pad(text: &str, width: i64, fill: &str, before: bool) -> Result<String, String> {
    let have = text.chars().count() as i64;
    if width <= have || fill.is_empty() {
        return Ok(text.to_string());
    }
    if width > MAX_PADDED {
        return Err(format!(
            "cannot pad a string to {width} characters; the most is {MAX_PADDED}"
        ));
    }
    let padding = fill.chars().cycle().take((width - have) as usize);
    let mut padded = String::with_capacity(text.len() + (width - have) as usize);
    match before {
        true => {
            padded.extend(padding);
            padded.push_str(text);
        }
        false => {
            padded.push_str(text);
            padded.extend(padding);
        }
    }
    Ok(padded)
}

/// Where `part` first occurs in `text`, counted in characters from 0.
pub fn index_of(text: &str, part: &str) -> Option<i64> {
    let at = text.find(part)?;
    Some(text[..at].chars().count() as i64)
}

/// The truth value that `text` spells out, in any letter case and with space around it.
pub fn to_bool(text: &str) -> Option<bool> {
    let text = text.trim();
    let is = |words: &[&str]| words.iter().any(|word| text.eq_ignore_ascii_case(word));
    if is(&["true", "t", "yes", "y", "1"]) {
        Some(true)
    } else if is(&["false", "f", "no", "n", "0"]) {
        Some(false)
    } else {
        None
    }
}

/// A number as people write it: with separators of thousands (`1,234.5`), a percent sign
/// (`45%` is 0.45), or parentheses for a negative amount (`(120)`). `None` for anything else.
pub fn parse_number(text: &str) -> Option<f64> {
    let mut text = text.trim();
    let mut negative = false;
    if let Some(inner) = text
        .strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
    {
        negative = true;
        text = inner.trim();
    }
    let percent = match text.strip_suffix('%') {
        Some(rest) => {
            text = rest.trim_end();
            true
        }
        None => false,
    };
    match text.as_bytes().first() {
        Some(b'-') => {
            negative = !negative;
            text = &text[1..];
        }
        Some(b'+') => text = &text[1..],
        _ => {}
    }
    // Only digits, separators between them, one point, and an exponent make a number: this
    // keeps out the words that the float parser also takes, such as `inf` and `nan`.
    let starts = text
        .as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_digit() || *byte == b'.');
    let plain = text
        .bytes()
        .all(|byte| byte.is_ascii_digit() || b",._eE+-".contains(&byte));
    if !starts || !plain || text.contains(",,") || text.ends_with(',') {
        return None;
    }
    let digits: String = text.chars().filter(|c| !matches!(c, ',' | '_')).collect();
    let mut value: f64 = digits.parse().ok()?;
    if percent {
        value /= 100.0;
    }
    Some(if negative { -value } else { value })
}

/// Compiles a regular expression. The error says what is wrong with `pattern` on one line.
pub fn regex(pattern: &str) -> Result<Regex, String> {
    Regex::new(pattern).map_err(|err| {
        // The library draws the pattern with a marker under the fault, over several lines.
        let text = err.to_string();
        let reason = text.rsplit("error: ").next().unwrap_or(&text).trim();
        format!("`{pattern}` is not a regular expression: {reason}")
    })
}

/// Checks that `regex` has a group numbered `group`, where 0 is the whole match.
pub fn regex_group(regex: &Regex, group: i64) -> Result<usize, String> {
    let groups = regex.captures_len() - 1;
    match usize::try_from(group) {
        Ok(group) if group <= groups => Ok(group),
        _ => {
            let has = match groups {
                0 => "no groups".to_string(),
                1 => "1 group".to_string(),
                n => format!("{n} groups"),
            };
            Err(format!(
                "there is no group {group}: `{}` has {has}",
                regex.as_str()
            ))
        }
    }
}

/// The first match of `regex` in `text`, or the part of it that group `group` matched.
pub fn regex_extract<'t>(regex: &Regex, text: &'t str, group: usize) -> Option<&'t str> {
    Some(regex.captures(text)?.get(group)?.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substrings_count_characters() {
        assert_eq!(substring("hello", 1, Some(3)), "ell");
        assert_eq!(substring("hello", 2, None), "llo");
        assert_eq!(substring("hello", -3, None), "llo");
        assert_eq!(substring("hello", -3, Some(2)), "ll");
        assert_eq!(substring("hello", -9, Some(2)), "he");
        assert_eq!(substring("hello", 9, None), "");
        assert_eq!(substring("hello", 3, Some(99)), "lo");
        assert_eq!(substring("hello", 1, Some(-1)), "");
        assert_eq!(substring("hello", i64::MIN, Some(i64::MAX)), "hello");
        assert_eq!(substring("größe", 2, Some(2)), "öß");
        assert_eq!(substring("", 0, None), "");
    }

    #[test]
    fn splitting_and_replacing_leave_an_empty_pattern_alone() {
        assert_eq!(replace("a-b-c", "-", "+"), "a+b+c");
        assert_eq!(replace("abc", "", "+"), "abc");
        assert_eq!(split("a,b,,c", ","), ["a", "b", "", "c"]);
        assert_eq!(split("abc", ""), ["abc"]);
        assert_eq!(split("", ","), [""]);
        assert_eq!(split_part("a,b,c", ",", 0), Some("a"));
        assert_eq!(split_part("a,b,c", ",", 2), Some("c"));
        assert_eq!(split_part("a,b,c", ",", 3), None);
        assert_eq!(split_part("a,b,c", ",", -1), Some("c"));
        assert_eq!(split_part("a,b,c", ",", -3), Some("a"));
        assert_eq!(split_part("a,b,c", ",", -4), None);
        assert_eq!(split_part("a,b,c", ",", i64::MIN), None);
        assert_eq!(split_part("abc", "", 0), Some("abc"));
        assert_eq!(split_part("abc", "", 1), None);
    }

    #[test]
    fn padding_repeats_and_cuts_the_fill() {
        assert_eq!(pad("7", 3, "0", true).unwrap(), "007");
        assert_eq!(pad("7", 3, "0", false).unwrap(), "700");
        assert_eq!(pad("ab", 7, "xyz", true).unwrap(), "xyzxyab");
        assert_eq!(pad("hello", 3, "0", true).unwrap(), "hello");
        assert_eq!(pad("a", 3, "", true).unwrap(), "a");
        assert_eq!(pad("é", 3, "ü", false).unwrap(), "éüü");
        assert!(pad("a", MAX_PADDED + 1, " ", true).is_err());
    }

    #[test]
    fn positions_are_in_characters() {
        assert_eq!(index_of("größe", "ß"), Some(3));
        assert_eq!(index_of("abc", ""), Some(0));
        assert_eq!(index_of("abc", "x"), None);
    }

    #[test]
    fn numbers_are_read_as_people_write_them() {
        assert_eq!(parse_number("1,234.50"), Some(1234.5));
        assert_eq!(parse_number(" 45% "), Some(0.45));
        assert_eq!(parse_number("(120)"), Some(-120.0));
        assert_eq!(parse_number("-1,000"), Some(-1000.0));
        assert_eq!(parse_number("(-5)"), Some(5.0));
        assert_eq!(parse_number("+7"), Some(7.0));
        assert_eq!(parse_number(".5"), Some(0.5));
        assert_eq!(parse_number("1e3"), Some(1000.0));
        assert_eq!(parse_number("1_000"), Some(1000.0));
        for bad in [
            "", "abc", "12abc", "nan", "inf", "1,,2", "1,", "%", "--1", "1.2.3", "()",
        ] {
            assert_eq!(parse_number(bad), None, "{bad:?}");
        }
        assert_eq!(to_bool(" Yes "), Some(true));
        assert_eq!(to_bool("FALSE"), Some(false));
        assert_eq!(to_bool("0"), Some(false));
        assert_eq!(to_bool("2"), None);
        assert_eq!(to_bool(""), None);
    }

    #[test]
    fn regex_errors_fit_on_one_line() {
        let message = regex("a(b").unwrap_err();
        assert_eq!(message, "`a(b` is not a regular expression: unclosed group");
        let regex = regex(r"(\d+)-(\d+)").unwrap();
        assert_eq!(regex_group(&regex, 2), Ok(2));
        assert_eq!(
            regex_group(&regex, 3).unwrap_err(),
            r"there is no group 3: `(\d+)-(\d+)` has 2 groups"
        );
        assert!(regex_group(&regex, -1).is_err());
        assert_eq!(regex_extract(&regex, "at 12-34.", 0), Some("12-34"));
        assert_eq!(regex_extract(&regex, "at 12-34.", 2), Some("34"));
        assert_eq!(regex_extract(&regex, "none", 0), None);
    }
}
