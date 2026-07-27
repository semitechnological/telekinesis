use ansi_to_tui::IntoText;
use crepuscularity_tui::ratatui::text::{Line, Text};
use glamour::{Renderer, Style};

pub fn render(markdown: &str, width: usize) -> Vec<Line<'static>> {
    let mut styles = Style::TokyoNight.config();
    styles.document.margin = Some(0);
    styles.document.style.block_prefix.clear();
    styles.document.style.block_suffix.clear();
    let output = Renderer::new()
        .with_style_config(styles)
        .with_word_wrap(width.max(1))
        .with_preserved_newlines(true)
        .render(markdown);

    output
        .into_text()
        .unwrap_or_else(|_| Text::raw(output))
        .lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retains_full_multiline_content() {
        let lines = render(
            "# Status\n\nFirst paragraph survives.\n\n- alpha\n- omega\n\nFinal line survives.",
            80,
        );
        let content = lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(content.contains("Status"));
        assert!(content.contains("First paragraph survives."));
        assert!(content.contains("alpha"));
        assert!(content.contains("omega"));
        assert!(content.contains("Final line survives."));
    }

    #[test]
    fn styles_and_wraps_markdown() {
        let lines = render(
            "This is **important** and deliberately long enough to wrap across several terminal lines.",
            24,
        );

        assert!(lines.len() > 1);
        assert!(lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.style.fg.is_some() || !span.style.add_modifier.is_empty()));
    }
}
