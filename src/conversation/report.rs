//! What git-agent tells the user about the model it uses and how much of the changes fit.

use super::fit::Kept;
use super::messages::Fit;
use super::models::{ContextSource, Model, Pick};
use crate::safe_text;
use crate::ui::{self, thousands};

const CONTEXT_TRIMMED: &str = "   To fit, git-agent left out the unchanged lines around the \
                               changes; every changed line was sent.";

/// What git-agent says about how the changes fit, after it names the model.
#[derive(Debug, PartialEq)]
enum FitReport {
    /// All of the changes were sent, with the unchanged lines around them.
    WithContext,
    /// Every changed line was sent, without the unchanged lines around them.
    WithoutContext,
    /// Part of the changes was sent.
    Shortened {
        warning: String,
        left_out: Vec<String>,
        to_send_more: String,
    },
}

/// Says which model gets the changes and, if they didn't fit its context, what
/// was left out. `noun` names what the model writes; `smaller` says how to
/// send less at a time, if there is a way.
pub fn report_fit(model: &Model, fit: &Fit, noun: &str, smaller: Option<&str>) {
    report_model(model, fit.prompt_tokens);
    match FitReport::of(model, fit, noun, smaller) {
        FitReport::WithContext => {}
        FitReport::WithoutContext => ui::info(ui::dim(CONTEXT_TRIMMED)),
        FitReport::Shortened {
            warning,
            left_out,
            to_send_more,
        } => {
            ui::warn(warning);
            left_out.into_iter().for_each(ui::info);
            ui::info(ui::dim(&to_send_more));
        }
    }
}

impl FitReport {
    fn of(model: &Model, fit: &Fit, noun: &str, smaller: Option<&str>) -> Self {
        if !fit.is_complete() {
            Self::Shortened {
                warning: shortened(fit),
                left_out: left_out(fit),
                to_send_more: how_to_send_more(model, noun, smaller),
            }
        } else if fit.context_trimmed {
            Self::WithoutContext
        } else {
            Self::WithContext
        }
    }
}

/// Says which model gets the prompt, and how much of its context the prompt takes.
pub fn report_model(model: &Model, prompt_tokens: usize) {
    ui::info(format!(
        "{} {}",
        model_named(model),
        ui::dim(&context_taken(model, prompt_tokens))
    ));
    if let Some(note) = assumed_context(model) {
        ui::info(ui::dim(&note));
    }
}

/// The model, and why git-agent picked it if it wasn't configured.
fn model_named(model: &Model) -> String {
    let chosen = match model.picked {
        Some(Pick::Loaded) => " (loaded on the server)",
        Some(Pick::Only) => " (the server's only model)",
        None => "",
    };
    format!("🤖 {}{chosen}", safe_text::one_line(&model.id))
}

fn context_taken(model: &Model, prompt_tokens: usize) -> String {
    format!(
        "· prompt about {} of {} context tokens",
        thousands(prompt_tokens),
        thousands(model.context_length)
    )
}

/// A note on the context length git-agent assumed, if nothing reported one.
fn assumed_context(model: &Model) -> Option<String> {
    (model.context_source == ContextSource::Assumed).then(|| {
        format!(
            "   The server didn't report the model's context length, so git-agent assumes {} \
             tokens.\n   If the model has more, set `context_length` in the config file or pass \
             --context-length.",
            thousands(model.context_length)
        )
    })
}

fn shortened(fit: &Fit) -> String {
    format!(
        "the changes need about {} tokens, more than fit in the context, so git-agent sent part \
         of them:",
        thousands(fit.full_tokens)
    )
}

/// What was left out of changes that didn't fit, a line each.
fn left_out(fit: &Fit) -> Vec<String> {
    let whole = fit.files.iter().filter(|file| file.is_whole()).count();
    let cut = cut_short(fit);
    let dropped = by_name_only(fit);
    [
        Some(format!(
            "   {whole} of {} files in full, without the unchanged lines around the changes",
            fit.files.len()
        )),
        (!cut.is_empty()).then(|| format!("   {} cut short: {}", cut.len(), first_few(&cut))),
        (!dropped.is_empty())
            .then(|| format!("   {} by name only: {}", dropped.len(), first_few(&dropped))),
        (fit.list_lines_left_out > 0).then(|| {
            format!(
                "   {} files left out of the file list too",
                fit.list_lines_left_out
            )
        }),
        fit.commit_bodies_left_out.then(|| {
            "   the commits by their subjects only, without the rest of their messages".to_owned()
        }),
        (fit.commits_left_out > 0).then(|| {
            format!(
                "   {} commits left out of the commit list",
                fit.commits_left_out
            )
        }),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The files cut short, with how many of their changed lines were sent.
fn cut_short(fit: &Fit) -> Vec<String> {
    fit.files
        .iter()
        .filter(|file| file.kept == Kept::Start)
        .map(|file| {
            format!(
                "{} ({} of {} changed lines)",
                file.path, file.kept_changed_lines, file.changed_lines
            )
        })
        .collect()
}

/// The files sent by name only.
fn by_name_only(fit: &Fit) -> Vec<String> {
    fit.files
        .iter()
        .filter(|file| file.is_dropped())
        .map(|file| file.path.clone())
        .collect()
}

fn how_to_send_more(model: &Model, noun: &str, smaller: Option<&str>) -> String {
    let larger_context = match model.context_source {
        ContextSource::Server => "load the model with a larger context length",
        ContextSource::Config => "raise `context_length` if the model allows more",
        ContextSource::Assumed => "set `context_length` if the model has more",
    };
    let ways = match smaller {
        Some(smaller) => format!("{smaller}, or {larger_context}"),
        None => larger_context.to_owned(),
    };
    format!("   The {noun} may miss details from these files. To send more, {ways}.")
}

/// The first three items, then how many more there are.
fn first_few(items: &[String]) -> String {
    const SHOWN: usize = 3;
    let mut text = items[..items.len().min(SHOWN)].join(", ");
    if items.len() > SHOWN {
        text.push_str(&format!(", and {} more", items.len() - SHOWN));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::fit::FileFit;
    use crate::conversation::test_support::configured;

    fn model(picked: Option<Pick>, context_source: ContextSource) -> Model {
        Model {
            id: "qwen\u{1b}[2J".to_owned(),
            picked,
            context_source,
            ..configured(32_768)
        }
    }

    fn file(path: &str, kept: Kept, kept_changed_lines: usize) -> FileFit {
        FileFit {
            path: path.to_owned(),
            kept,
            changed_lines: 40,
            kept_changed_lines,
        }
    }

    fn fit(files: Vec<FileFit>, context_trimmed: bool) -> Fit {
        Fit {
            prompt_tokens: 1000,
            full_tokens: 123_456,
            files,
            context_trimmed,
            list_lines_left_out: 0,
            commit_bodies_left_out: false,
            commits_left_out: 0,
        }
    }

    fn texts(items: &[&str]) -> Vec<String> {
        items.iter().map(|&item| item.to_owned()).collect()
    }

    const WARNING: &str = "the changes need about 123,456 tokens, more than fit in the context, \
                           so git-agent sent part of them:";

    #[test]
    fn names_the_model_escaped_and_why_it_was_picked() {
        let named = |picked| model_named(&model(picked, ContextSource::Server));
        assert_eq!(named(None), "🤖 qwen\\033[2J");
        assert_eq!(
            named(Some(Pick::Loaded)),
            "🤖 qwen\\033[2J (loaded on the server)"
        );
        assert_eq!(
            named(Some(Pick::Only)),
            "🤖 qwen\\033[2J (the server's only model)"
        );
        assert_eq!(
            context_taken(&model(None, ContextSource::Server), 1234),
            "· prompt about 1,234 of 32,768 context tokens"
        );
    }

    #[test]
    fn notes_an_assumed_context_only() {
        assert_eq!(assumed_context(&model(None, ContextSource::Server)), None);
        assert_eq!(assumed_context(&model(None, ContextSource::Config)), None);
        assert_eq!(
            assumed_context(&model(None, ContextSource::Assumed)).as_deref(),
            Some(
                "   The server didn't report the model's context length, so git-agent assumes \
                 32,768 tokens.\n   If the model has more, set `context_length` in the config \
                 file or pass --context-length."
            )
        );
    }

    #[test]
    fn says_nothing_more_when_every_line_was_sent() {
        let report = |trimmed| {
            let fit = fit(vec![file("a.rs", Kept::Whole, 40)], trimmed);
            FitReport::of(&model(None, ContextSource::Server), &fit, "message", None)
        };
        assert_eq!(report(false), FitReport::WithContext);
        assert_eq!(report(true), FitReport::WithoutContext);
    }

    #[test]
    fn lists_each_way_the_changes_were_shortened() {
        let fit = Fit {
            list_lines_left_out: 1,
            commit_bodies_left_out: true,
            commits_left_out: 1,
            ..fit(
                vec![
                    file("whole.rs", Kept::Whole, 40),
                    file("a.rs", Kept::Start, 10),
                    file("c.rs", Kept::Nothing, 0),
                ],
                true,
            )
        };
        assert_eq!(
            FitReport::of(&model(None, ContextSource::Server), &fit, "message", None),
            FitReport::Shortened {
                warning: WARNING.to_owned(),
                left_out: texts(&[
                    "   1 of 3 files in full, without the unchanged lines around the changes",
                    "   1 cut short: a.rs (10 of 40 changed lines)",
                    "   1 by name only: c.rs",
                    "   1 files left out of the file list too",
                    "   the commits by their subjects only, without the rest of their messages",
                    "   1 commits left out of the commit list",
                ]),
                to_send_more: "   The message may miss details from these files. To send more, \
                               load the model with a larger context length."
                    .to_owned(),
            }
        );
    }

    #[test]
    fn reports_commits_by_their_subjects_alone_as_shortened() {
        let fit = Fit {
            commit_bodies_left_out: true,
            ..fit(vec![file("a.rs", Kept::Whole, 40)], false)
        };
        let FitReport::Shortened { left_out, .. } =
            FitReport::of(&model(None, ContextSource::Server), &fit, "message", None)
        else {
            panic!("the fit is shortened");
        };
        assert_eq!(
            left_out,
            [
                "   1 of 1 files in full, without the unchanged lines around the changes",
                "   the commits by their subjects only, without the rest of their messages",
            ]
        );
    }

    #[test]
    fn suggests_a_larger_context_the_way_it_was_set() {
        let ways = |source, smaller| how_to_send_more(&model(None, source), "message", smaller);
        const MISSES: &str = "   The message may miss details from these files. To send more,";
        assert_eq!(
            ways(ContextSource::Server, None),
            format!("{MISSES} load the model with a larger context length.")
        );
        assert_eq!(
            ways(ContextSource::Config, None),
            format!("{MISSES} raise `context_length` if the model allows more.")
        );
        assert_eq!(
            ways(ContextSource::Assumed, Some("stage fewer files")),
            format!("{MISSES} stage fewer files, or set `context_length` if the model has more.")
        );
    }

    #[test]
    fn names_the_first_three_and_counts_the_rest() {
        assert_eq!(first_few(&[]), "");
        assert_eq!(first_few(&texts(&["a"])), "a");
        assert_eq!(first_few(&texts(&["a", "b", "c"])), "a, b, c");
        assert_eq!(
            first_few(&texts(&["a", "b", "c", "d"])),
            "a, b, c, and 1 more"
        );
        assert_eq!(
            first_few(&texts(&["a", "b", "c", "d", "e"])),
            "a, b, c, and 2 more"
        );
    }
}
