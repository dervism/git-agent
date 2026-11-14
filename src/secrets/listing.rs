//! How the secret checks show what they found: every finding by its place, and by its line where
//! it has one, with the rest of a crowded place on one line; and every file too large to check.

use super::finding::{Change, Finding, Place, Unchecked};
use crate::ui;
use std::collections::{HashMap, HashSet};

/// How many findings of one place a list names on lines of their own, or one more when only one
/// would be left. The rest of the place share one line that names their lines and kinds.
const NAMED_PER_PLACE: usize = 10;

/// Every finding of `found`, by place in the order each first appears: [`NAMED_PER_PLACE`] of a
/// place on lines of their own, then the rest of it on one line.
pub(super) fn findings<'a>(found: impl IntoIterator<Item = &'a Finding>) -> String {
    let lines: Vec<String> = by_place(found).iter().flat_map(Group::lines).collect();
    lines.join("\n")
}

/// Every file of `files`, one per line, with its size.
pub(super) fn unchecked(files: &[Unchecked]) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    let lines: Vec<String> = files
        .iter()
        .map(|file| format!("   {}  ({:.1} MiB)", file.place, file.size as f64 / MIB))
        .collect();
    lines.join("\n")
}

/// The findings of one place, as a list shows them: those it names on lines of their own, and the
/// rest.
struct Group<'a> {
    place: &'a Place,
    on_own_lines: Vec<&'a Finding>,
    rest: Vec<&'a Finding>,
}

impl<'a> Group<'a> {
    /// `found`, all at `place`: every one on a line of its own unless there are more than one over
    /// [`NAMED_PER_PLACE`], so that the rest is never one finding.
    fn of(place: &'a Place, mut found: Vec<&'a Finding>) -> Self {
        let named = if found.len() > NAMED_PER_PLACE + 1 {
            NAMED_PER_PLACE
        } else {
            found.len()
        };
        let rest = found.split_off(named);
        Self {
            place,
            on_own_lines: found,
            rest,
        }
    }

    fn lines(&self) -> impl Iterator<Item = String> + '_ {
        let own = self.on_own_lines.iter().map(|finding| own_line(finding));
        own.chain(self.rest_line())
    }

    /// The line that names the rest: how many, the lines they are on, and their kinds.
    fn rest_line(&self) -> Option<String> {
        (!self.rest.is_empty()).then(|| {
            format!(
                "   … and {} more in {}{}: {}",
                ui::thousands(self.rest.len()),
                self.place,
                on_lines(self.rest.iter().filter_map(|finding| finding.line)),
                kinds(&self.rest)
            )
        })
    }
}

/// `findings` by place, in the order each place first appears, keeping their order within a place.
fn by_place<'a>(findings: impl IntoIterator<Item = &'a Finding>) -> Vec<Group<'a>> {
    let mut index: HashMap<&Place, usize> = HashMap::new();
    let mut places: Vec<(&Place, Vec<&Finding>)> = Vec::new();
    for finding in findings {
        let at = *index.entry(&finding.place).or_insert_with(|| {
            places.push((&finding.place, Vec::new()));
            places.len() - 1
        });
        places[at].1.push(finding);
    }
    places
        .into_iter()
        .map(|(place, found)| Group::of(place, found))
        .collect()
}

/// A finding on a line of its own: its place, its line where it has one, its kind, and what the
/// change does to it.
fn own_line(finding: &Finding) -> String {
    let place = match finding.line {
        Some(line) => format!("{}:{line}", finding.place),
        None => finding.place.to_string(),
    };
    let change = match finding.change {
        Change::Added => "",
        Change::Unchanged => ", on an unchanged line",
        Change::Removed => ", on a removed line",
    };
    format!("   {place}  {}{change}", finding.kind)
}

/// `, on lines 11-14, 20` for the line numbers `lines`, in order and in runs, `, on line 12` for
/// one, and nothing for none.
fn on_lines(lines: impl Iterator<Item = usize>) -> String {
    let mut numbers: Vec<usize> = lines.collect();
    numbers.sort_unstable();
    numbers.dedup();
    let runs = runs(&numbers);
    match runs.as_slice() {
        [] => String::new(),
        [(first, last)] if first == last => format!(", on line {first}"),
        _ => {
            let written: Vec<String> = runs.iter().map(|&run| written_run(run)).collect();
            format!(", on lines {}", written.join(", "))
        }
    }
}

/// `numbers`, in order and without repeats, as runs of consecutive numbers, each as its first and
/// its last.
fn runs(numbers: &[usize]) -> Vec<(usize, usize)> {
    numbers.iter().fold(Vec::new(), |mut runs, &number| {
        match runs.last_mut() {
            Some((_, last)) if *last + 1 == number => *last = number,
            _ => runs.push((number, number)),
        }
        runs
    })
}

fn written_run((first, last): (usize, usize)) -> String {
    if first == last {
        first.to_string()
    } else {
        format!("{first}-{last}")
    }
}

/// The kinds of `findings`, each once, in the order they first appear.
fn kinds(findings: &[&Finding]) -> String {
    let mut seen = HashSet::new();
    let kinds: Vec<&str> = findings
        .iter()
        .map(|finding| finding.kind.as_str())
        .filter(|kind| seen.insert(*kind))
        .collect();
    kinds.join(", ")
}

#[cfg(test)]
mod tests {
    use super::super::finding::FileRef;
    use super::super::finding::test_support::{
        every_kind_of_finding, every_kind_of_place, in_commit, print_of,
    };
    use super::*;
    use crate::git::CommitId;

    fn found(place: &Place, line: Option<usize>, kind: &str) -> Finding {
        Finding {
            place: place.clone(),
            line,
            change: Change::Added,
            kind: kind.to_owned(),
            print: print_of("a GitHub token", false),
        }
    }

    /// GitHub tokens on the lines `lines` of `place`.
    fn tokens(place: &Place, lines: impl IntoIterator<Item = usize>) -> Vec<Finding> {
        lines
            .into_iter()
            .map(|line| found(place, Some(line), "GitHub token"))
            .collect()
    }

    /// The places of `findings`, each once, in the order they first appear, told apart by `Place`.
    fn places_in_order(findings: &[Finding]) -> Vec<&Place> {
        findings.iter().fold(Vec::new(), |mut places, finding| {
            if !places.contains(&&finding.place) {
                places.push(&finding.place);
            }
            places
        })
    }

    /// What grouping promises for `findings`: one group per place, as `Place` tells them apart, in
    /// the order the places first appear; each group holds every finding of its place, the same
    /// ones in the same order; ten of them on lines of their own, or all when that leaves at most
    /// one, so that the rest is never a single finding.
    fn assert_grouped(findings: &[Finding]) -> Vec<Group<'_>> {
        let groups = by_place(findings);
        let places: Vec<&Place> = groups.iter().map(|group| group.place).collect();
        assert_eq!(places, places_in_order(findings));
        for group in &groups {
            let own: Vec<&Finding> = findings
                .iter()
                .filter(|finding| finding.place == *group.place)
                .collect();
            let kept: Vec<&Finding> = group
                .on_own_lines
                .iter()
                .chain(&group.rest)
                .copied()
                .collect();
            assert_eq!(kept.len(), own.len(), "{}", group.place);
            let same = kept.iter().zip(&own).all(|(a, b)| std::ptr::eq(*a, *b));
            assert!(same, "{}", group.place);
            assert_ne!(group.rest.len(), 1, "{}", group.place);
            assert!(
                group.on_own_lines.len() <= NAMED_PER_PLACE + 1,
                "{}",
                group.place
            );
            let full = group.on_own_lines.len() == NAMED_PER_PLACE;
            assert!(group.rest.is_empty() || full, "{}", group.place);
        }
        groups
    }

    /// However the places come interleaved, each place's findings are grouped in their order, the
    /// first ten on lines of their own, or all eleven when one would be left.
    #[test]
    fn groups_every_finding_by_its_place() {
        let places = every_kind_of_place();
        let cases = [(1, 1), (10, 10), (11, 11), (12, 10), (25, 10)];
        for (count, named) in cases {
            let findings: Vec<Finding> = (1..=count)
                .flat_map(|line| places.iter().map(move |place| (place, line)))
                .map(|(place, line)| found(place, Some(line), "GitHub token"))
                .collect();
            let groups = assert_grouped(&findings);
            assert_eq!(groups.len(), places.len());
            for group in &groups {
                assert_eq!(group.on_own_lines.len(), named, "{count}: {}", group.place);
            }
        }
        assert!(assert_grouped(&[]).is_empty());
    }

    /// Findings of every kind, which come with their places interleaved, group as they should.
    #[test]
    fn groups_findings_of_every_kind() {
        let findings = every_kind_of_finding();
        let groups = assert_grouped(&findings);
        assert!(
            groups
                .iter()
                .all(|group| group.on_own_lines.len() == NAMED_PER_PLACE)
        );
    }

    /// A file and other text of the same name are two places, though they read alike.
    #[test]
    fn tells_places_apart_that_read_alike() {
        let file = Place::file("a.env");
        let text = Place::text("a.env");
        assert_eq!(file.to_string(), text.to_string());
        let alike: Vec<Finding> = tokens(&file, 1..=6)
            .into_iter()
            .chain(tokens(&text, 1..=6))
            .collect();
        assert_eq!(assert_grouped(&alike).len(), 2);
        assert_eq!(findings(&alike).lines().count(), 12);
    }

    #[test]
    fn lists_each_finding_with_its_place_and_what_the_change_does_to_it() {
        let commit = CommitId::of("1a2b3c4");
        let listed = [
            Finding {
                change: Change::Added,
                ..found(&Place::file("src/a.rs"), Some(3), "GitHub token")
            },
            Finding {
                change: Change::Unchanged,
                ..found(&Place::Message(Some(commit)), None, "GitHub token")
            },
            Finding {
                change: Change::Removed,
                ..found(&Place::file("b.env"), Some(9), "GitHub token")
            },
        ];
        assert_eq!(
            findings(&listed),
            "   src/a.rs:3  GitHub token\n   commit 1a2b3c4 message  GitHub token, on an \
             unchanged line\n   b.env:9  GitHub token, on a removed line"
        );
        assert_eq!(findings(&[]), "");
    }

    /// Up to eleven findings of a place are each named on a line of their own.
    #[test]
    fn names_each_finding_of_a_place_that_has_up_to_eleven() {
        for count in [1, 8, 10, 11] {
            let listed = findings(&tokens(&Place::file("a.env"), 1..=count));
            let each: Vec<String> = (1..=count)
                .map(|line| format!("   a.env:{line}  GitHub token"))
                .collect();
            assert_eq!(listed, each.join("\n"), "{count}");
        }
    }

    /// Past ten, a place's findings are named on one line, by their lines and kinds.
    #[test]
    fn names_the_rest_of_a_crowded_place_by_their_lines_and_kinds() {
        let place = Place::file("fixtures/keys.txt");
        let mut crowded = tokens(&place, 1..=2000);
        crowded[1500].kind = "private key".to_owned();

        let listed = findings(&crowded);

        let lines: Vec<&str> = listed.lines().collect();
        assert_eq!(lines.len(), 11);
        assert_eq!(lines[9], "   fixtures/keys.txt:10  GitHub token");
        assert_eq!(
            lines[10],
            "   … and 1,990 more in fixtures/keys.txt, on lines 11-2000: GitHub token, private key"
        );
    }

    /// The lines of the rest are written in order, in runs, each once.
    #[test]
    fn writes_the_lines_of_the_rest_in_runs() {
        let place = Place::file("a.env");
        let cases: [(&[usize], &str); 6] = [
            (&[11, 12, 13, 14, 20, 31], ", on lines 11-14, 20, 31"),
            (&[31, 11, 20, 12], ", on lines 11-12, 20, 31"),
            (&[5, 7], ", on lines 5, 7"),
            (&[12, 12], ", on line 12"),
            (&[12, 12, 13], ", on lines 12-13"),
            (&[3, 2, 1], ", on lines 1-3"),
        ];
        for (rest, written) in cases {
            let crowded: Vec<Finding> = tokens(&place, 1..=10)
                .into_iter()
                .chain(tokens(&place, rest.iter().copied()))
                .collect();
            let listed = findings(&crowded);
            let last = format!(
                "   … and {} more in a.env{written}: GitHub token",
                rest.len()
            );
            assert_eq!(listed.lines().last(), Some(last.as_str()), "{rest:?}");
        }
    }

    /// A place without line numbers, such as a commit's message, names the kinds of the rest.
    #[test]
    fn names_the_kinds_of_the_rest_of_a_place_without_lines() {
        let message = Place::Message(Some(CommitId::of("1a2b3c4")));
        let kinds = ["GitHub token", "AWS access key", "GitHub token"];
        let crowded: Vec<Finding> = (0..10)
            .map(|_| found(&message, None, "private key"))
            .chain(kinds.iter().map(|kind| found(&message, None, kind)))
            .collect();

        let listed = findings(&crowded);

        let last = "   … and 3 more in commit 1a2b3c4 message: GitHub token, AWS access key";
        assert_eq!(listed.lines().last(), Some(last));
    }

    /// The line of the rest names every kind of place as the place reads.
    #[test]
    fn names_every_kind_of_place_on_the_line_of_the_rest() {
        for place in every_kind_of_place() {
            let listed = findings(&tokens(&place, 1..=12));
            let last = format!("   … and 2 more in {place}, on lines 11-12: GitHub token");
            assert_eq!(listed.lines().last(), Some(last.as_str()));
        }
    }

    /// Places keep the order they first appear in, each with its findings together.
    #[test]
    fn lists_a_place_s_findings_together_where_they_first_appear() {
        let (a, b) = (Place::file("a.env"), in_commit("1a2b3c4", "b.env"));
        let interleaved = [
            found(&a, Some(1), "GitHub token"),
            found(&b, Some(7), "AWS access key"),
            found(&a, Some(2), "GitHub token"),
        ];
        assert_eq!(
            findings(&interleaved),
            "   a.env:1  GitHub token\n   a.env:2  GitHub token\n   commit 1a2b3c4 b.env:7  AWS \
             access key"
        );
    }

    /// Every file too large to check is named, however many there are.
    #[test]
    fn names_every_file_too_large_to_check() {
        let files: Vec<Unchecked> = (1..=7)
            .map(|n| Unchecked {
                place: FileRef::changed(format!("pkg{n}/data.bin")),
                size: (16 + n) * 1024 * 1024,
                blob: n.to_string().repeat(40),
            })
            .collect();
        let each: Vec<String> = (1..=7)
            .map(|n| format!("   pkg{n}/data.bin  ({}.0 MiB)", 16 + n))
            .collect();
        assert_eq!(unchecked(&files), each.join("\n"));
        assert_eq!(unchecked(&[]), "");
    }
}
