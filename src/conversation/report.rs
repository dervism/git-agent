//! What git-agent tells the user about the model it uses and how much of the changes fit.

use super::Fit;
use super::fit::Kept;
use super::models::{ContextSource, Model, Pick};
use crate::safe_text;
use crate::ui::{self, thousands};

/// Says which model gets the changes and, if they didn't fit its context, what
/// was left out. `noun` names what the model writes; `smaller` says how to
/// send less at a time, if there is a way.
pub fn report_fit(model: &Model, fit: &Fit, noun: &str, smaller: Option<&str>) {
    report_model(model, fit.prompt_tokens);
    if fit.is_complete() {
        if fit.context_trimmed {
            ui::info(ui::dim(
                "   To fit, git-agent left out the unchanged lines around the changes; \
                 every changed line was sent.",
            ));
        }
        return;
    }
    report_shortened(model, fit, noun, smaller);
}

/// Says which model gets the prompt, and how much of its context the prompt takes.
pub fn report_model(model: &Model, prompt_tokens: usize) {
    let chosen = match model.picked {
        Some(Pick::Loaded) => " (loaded on the server)",
        Some(Pick::Only) => " (the server's only model)",
        None => "",
    };
    ui::info(format!(
        "🤖 {}{chosen} {}",
        safe_text::one_line(&model.id),
        ui::dim(&format!(
            "· prompt about {} of {} context tokens",
            thousands(prompt_tokens),
            thousands(model.context_length)
        ))
    ));
    if model.context_source == ContextSource::Assumed {
        ui::info(ui::dim(&format!(
            "   The server didn't report the model's context length, so git-agent assumes {} \
             tokens.\n   If the model has more, set `context_length` in the config file or pass \
             --context-length.",
            thousands(model.context_length)
        )));
    }
}

/// Lists what was left out of changes that didn't fit, and how to send more.
pub fn report_shortened(model: &Model, fit: &Fit, noun: &str, smaller: Option<&str>) {
    ui::warn(format!(
        "the changes need about {} tokens, more than fit in the context, so git-agent sent \
         part of them:",
        thousands(fit.full_tokens)
    ));
    let whole = fit.files.iter().filter(|file| file.is_whole()).count();
    let cut: Vec<String> = fit
        .files
        .iter()
        .filter(|file| file.kept == Kept::Start)
        .map(|file| {
            format!(
                "{} ({} of {} changed lines)",
                file.path, file.kept_changed_lines, file.changed_lines
            )
        })
        .collect();
    let dropped: Vec<String> = fit
        .files
        .iter()
        .filter(|file| file.is_dropped())
        .map(|file| file.path.clone())
        .collect();
    ui::info(format!(
        "   {whole} of {} files in full, without the unchanged lines around the changes",
        fit.files.len()
    ));
    if !cut.is_empty() {
        ui::info(format!("   {} cut short: {}", cut.len(), first_few(&cut)));
    }
    if !dropped.is_empty() {
        ui::info(format!(
            "   {} by name only: {}",
            dropped.len(),
            first_few(&dropped)
        ));
    }
    if fit.list_lines_left_out > 0 {
        ui::info(format!(
            "   {} files left out of the file list too",
            fit.list_lines_left_out
        ));
    }
    if fit.commit_bodies_left_out {
        ui::info("   the commits by their subjects only, without the rest of their messages");
    }
    if fit.commits_left_out > 0 {
        ui::info(format!(
            "   {} commits left out of the commit list",
            fit.commits_left_out
        ));
    }
    let larger_context = match model.context_source {
        ContextSource::Server => "load the model with a larger context length",
        ContextSource::Config => "raise `context_length` if the model allows more",
        ContextSource::Assumed => "set `context_length` if the model has more",
    };
    let ways = match smaller {
        Some(smaller) => format!("{smaller}, or {larger_context}"),
        None => larger_context.to_owned(),
    };
    ui::info(ui::dim(&format!(
        "   The {noun} may miss details from these files. To send more, {ways}."
    )));
}

/// The first three items, then how many more there are.
pub fn first_few(items: &[String]) -> String {
    const SHOWN: usize = 3;
    let mut text = items[..items.len().min(SHOWN)].join(", ");
    if items.len() > SHOWN {
        text.push_str(&format!(", and {} more", items.len() - SHOWN));
    }
    text
}
