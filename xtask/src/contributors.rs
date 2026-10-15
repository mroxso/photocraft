//! `cargo xtask contributors [--check | --fix]`: keep `contributors/people.toml` sorted.
//!
//! Every contributor adds their own `[people.<username>]` table to that file, and appending them all
//! at the end makes any two of those PRs conflict on the same lines. Keeping the tables in
//! case-insensitive alphabetical order by username means a new entry lands where it sorts, so two
//! entries only collide when their usernames are neighbours.
//!
//! The About window does not use the file order (it sorts by first contribution, #369), so this is
//! purely about merge conflicts.
//!
//! Only the table order is touched: every line of an entry is kept byte-for-byte, so a re-sort stays
//! a permutation of the file's lines and `git blame` still points at whoever wrote each entry (the
//! one-time bulk sort goes in `.git-blame-ignore-revs`).
//!
//! `--check` fails when the file is out of order, malformed or lists a username twice; that is the CI
//! form. `--fix` writes the sorted file.

use std::fmt;
use std::path::Path;

/// Everything before the first table, plus the tables in file order.
#[derive(Debug, PartialEq, Eq)]
struct File {
    prelude: Vec<String>,
    entries: Vec<Entry>,
}

/// One `[people.<username>]` table: its username and the entry's lines without trailing blanks.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    username: String,
    lines: Vec<String>,
}

/// What is wrong with the file. `Unsorted` and `NoTrailingNewline` are what `--fix` can repair; the
/// rest need a human.
#[derive(Debug, PartialEq, Eq)]
enum Problem {
    Unsorted { expected_first: String },
    Duplicate(String),
    NotToml(String),
    NoTrailingNewline,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Problem::Unsorted { expected_first } => {
                write!(f, "the `[people.*]` tables are not in case-insensitive alphabetical order (expected {expected_first} first)")
            }
            Problem::Duplicate(username) => write!(f, "[people.{username}] is listed twice"),
            Problem::NotToml(e) => write!(f, "not valid TOML: {e}"),
            Problem::NoTrailingNewline => write!(f, "the file does not end with a newline"),
        }
    }
}

impl Problem {
    fn fixable(&self) -> bool {
        matches!(self, Problem::Unsorted { .. } | Problem::NoTrailingNewline)
    }
}

/// The sort key: case-insensitive by username, the exact username as a tie-break so the order is
/// total even when two names differ only in case.
fn sort_key(username: &str) -> (String, String) {
    (username.to_lowercase(), username.to_string())
}

/// The username in a `[people.<username>]` table header, if the line is one.
fn table_header(line: &str) -> Option<&str> {
    let rest = line.trim().strip_prefix("[people.")?.strip_suffix(']')?;
    (!rest.is_empty()).then_some(rest)
}

/// Split the file into its prelude and tables, keeping every line exactly as it is.
///
/// A comment block directly above a table header belongs to that table and travels with it; a blank
/// line separates a comment from the entry above it.
fn parse(text: &str) -> Result<File, String> {
    let lines: Vec<&str> = text.split('\n').collect();
    let Some(idx) = lines.iter().position(|l| l.trim_start().starts_with('[')) else {
        return Ok(File { prelude: lines.iter().map(|l| l.to_string()).collect(), entries: Vec::new() });
    };
    let mut prelude: Vec<String> = lines[..idx].iter().map(|l| l.to_string()).collect();
    let mut entries: Vec<Entry> = Vec::new();
    let mut current: Option<(String, Vec<String>)> = None;
    for line in &lines[idx..] {
        let trimmed = line.trim();
        match table_header(line) {
            Some(username) => {
                let mut body = take_heading(&mut current);
                body.push(line.to_string());
                if let Some((prev, mut prev_body)) = current.replace((username.to_string(), body)) {
                    trim_blanks(&mut prev_body);
                    entries.push(Entry { username: prev, lines: prev_body });
                }
            }
            // Any other table header is not ours to sort.
            None if trimmed.starts_with('[') => return Err(format!("unexpected table `{trimmed}`: only `[people.<username>]` tables belong here")),
            None => match &mut current {
                Some((_, body)) => body.push(line.to_string()),
                None => prelude.push(line.to_string()),
            },
        }
    }
    if let Some((username, mut body)) = current {
        trim_blanks(&mut body);
        entries.push(Entry { username, lines: body });
    }
    Ok(File { prelude, entries })
}

/// The comment lines directly above a new table header, moved out of the previous entry's body.
fn take_heading(current: &mut Option<(String, Vec<String>)>) -> Vec<String> {
    let Some((_, body)) = current else { return Vec::new() };
    let mut end = body.len();
    while body.get(end.saturating_sub(1)).is_some_and(|l| l.trim().is_empty()) {
        end -= 1;
    }
    let mut start = end;
    while body.get(start.saturating_sub(1)).is_some_and(|l| l.trim_start().starts_with('#')) {
        start -= 1;
    }
    if start == end {
        return Vec::new();
    }
    let heading: Vec<String> = body.drain(start..end).collect();
    trim_blanks(body);
    heading
}

fn trim_blanks(body: &mut Vec<String>) {
    while body.last().is_some_and(|l| l.trim().is_empty()) {
        body.pop();
    }
}

impl File {
    /// The file as it should be stored: prelude, then the tables in sorted order, one blank line
    /// between blocks, one trailing newline.
    fn render(&self) -> String {
        let mut prelude = self.prelude.clone();
        trim_blanks(&mut prelude);
        let mut out = prelude.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        let mut sorted: Vec<&Entry> = self.entries.iter().collect();
        sorted.sort_by_key(|e| sort_key(&e.username));
        for entry in sorted {
            out.push('\n');
            out.push_str(&entry.lines.join("\n"));
            out.push('\n');
        }
        out
    }

    fn problems(&self) -> Vec<Problem> {
        let mut problems = Vec::new();
        let mut seen: Vec<&str> = Vec::new();
        for entry in &self.entries {
            if seen.contains(&entry.username.as_str()) {
                problems.push(Problem::Duplicate(entry.username.clone()));
            }
            seen.push(&entry.username);
        }
        if !self.entries.windows(2).all(|w| sort_key(&w[0].username) <= sort_key(&w[1].username)) {
            let mut expected: Vec<(String, String)> = self.entries.iter().map(|e| sort_key(&e.username)).collect();
            expected.sort();
            let first = expected
                .into_iter()
                .zip(self.entries.iter())
                .find(|(k, e)| *k != sort_key(&e.username))
                .map_or_else(|| "the first entry".to_string(), |(k, _)| format!("`{}`", k.0));
            problems.push(Problem::Unsorted { expected_first: first });
        }
        problems
    }
}

/// The parsed file and everything wrong with it.
fn check_text(text: &str) -> Result<(File, Vec<Problem>), String> {
    let file = parse(text)?;
    let mut problems = file.problems();
    // A duplicated username is invalid TOML too; the duplicate check says it more clearly, so the
    // TOML check (which catches malformed entry content) only runs when the shape is otherwise good.
    if problems.is_empty()
        && let Err(e) = text.parse::<toml::Table>()
    {
        problems.push(Problem::NotToml(e.to_string()));
    }
    if !text.is_empty() && !text.ends_with('\n') {
        problems.push(Problem::NoTrailingNewline);
    }
    Ok((file, problems))
}

pub fn run(root: &Path, args: &[&str]) -> Result<(), String> {
    let fix = match args {
        [] | ["--check"] => false,
        ["--fix"] => true,
        other => return Err(format!("usage: cargo xtask contributors [--check | --fix] (got `{}`)", other.join(" "))),
    };
    let path = root.join("contributors/people.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let (file, problems) = check_text(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    if problems.iter().any(|p| !p.fixable()) {
        for p in &problems {
            eprintln!("{}: {p}", path.display());
        }
        return Err(format!("{} has problems `--fix` cannot repair", path.display()));
    }
    let sorted = file.render();
    if sorted == text {
        println!("OK: {} `[people.*]` entries, in order.", file.entries.len());
        return Ok(());
    }
    if fix {
        std::fs::write(&path, &sorted).map_err(|e| format!("{}: {e}", path.display()))?;
        println!("sorted {} entries in {}", file.entries.len(), path.display());
        return Ok(());
    }
    for p in &problems {
        eprintln!("{}: {p}", path.display());
    }
    if problems.is_empty() {
        println!("OK: {} `[people.*]` entries, in order (blank lines are not canonical; `--fix` normalizes them).", file.entries.len());
        return Ok(());
    }
    Err(format!("{} is not sorted (fix with `cargo xtask contributors --fix`)", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "# a comment\n\n";

    fn entry(username: &str) -> String {
        format!("[people.{username}]\nsync_github_name = true\n")
    }

    fn problems_of(text: &str) -> Vec<Problem> {
        check_text(text).map(|(_, p)| p).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn accepts_a_sorted_file() {
        let text = format!("{HEADER}{}{}", entry("ada"), entry("Bob"));
        assert_eq!(problems_of(&text), Vec::new());
    }

    #[test]
    fn rejects_the_wrong_order_and_names_the_expected_first_entry() {
        let text = format!("{HEADER}{}{}", entry("Bob"), entry("ada"));
        assert_eq!(problems_of(&text), vec![Problem::Unsorted { expected_first: "`ada`".into() }]);
    }

    #[test]
    fn fix_sorts_case_insensitively() {
        let text = format!("{HEADER}{}{}", entry("zebra"), entry("Apple"));
        let fixed = parse(&text).unwrap().render();
        assert!(fixed.find("[people.Apple]").unwrap() < fixed.find("[people.zebra]").unwrap(), "{fixed}");
        assert_eq!(problems_of(&fixed), Vec::new());
    }

    #[test]
    fn a_sort_only_moves_lines() {
        fn content(t: &str) -> Vec<&str> {
            let mut v: Vec<&str> = t.lines().filter(|l| !l.trim().is_empty()).collect();
            v.sort_unstable();
            v
        }
        let text = format!("{HEADER}{}{}\n{}", entry("zebra"), entry("Apple"), entry("ada"));
        let fixed = parse(&text).unwrap().render();
        assert_eq!(content(&text), content(&fixed), "re-sorting must not change or drop lines (blank separators aside)");
    }

    #[test]
    fn keeps_the_entry_body_and_its_comment() {
        let text = format!("{HEADER}[people.zebra]\nreal_name = \"Z.\"\n\n# mine\n[people.ada]\ndisplay_name = \"A\"\n");
        let fixed = parse(&text).unwrap().render();
        assert!(fixed.contains("# mine\n[people.ada]"), "{fixed}");
        assert!(fixed.contains("real_name = \"Z.\""), "{fixed}");
        assert_eq!(problems_of(&fixed), Vec::new());
    }

    #[test]
    fn rejects_a_duplicate_username() {
        let text = format!("{HEADER}{}{}", entry("ada"), entry("ada"));
        assert_eq!(problems_of(&text), vec![Problem::Duplicate("ada".into())]);
    }

    #[test]
    fn rejects_another_table_and_bad_toml() {
        assert!(parse(&format!("{HEADER}[other.ada]\n")).is_err());
        assert!(matches!(problems_of(&format!("{HEADER}{}x =\n", entry("ada")))[..], [Problem::NotToml(_)]));
    }

    #[test]
    fn a_file_without_entries_is_fine() {
        assert_eq!(problems_of(HEADER), Vec::new());
    }

    #[test]
    fn missing_trailing_newline_is_a_problem() {
        let text = format!("{HEADER}[people.ada]\ndisplay_name = \"A\"");
        assert_eq!(problems_of(&text), vec![Problem::NoTrailingNewline]);
    }

    #[test]
    fn the_real_file_is_sorted() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../contributors/people.toml");
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(problems_of(&text), Vec::new());
    }
}
