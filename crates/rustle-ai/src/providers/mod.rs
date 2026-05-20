//! Pluggable AI summarisation providers.

mod builtin;
mod llama_cpp;
mod ollama;

use rustle_core::{AiProvider, MeetingNotes, Settings};
use tracing::warn;

#[derive(Default)]
pub(crate) struct AiRuntime;

impl AiRuntime {
    pub(crate) fn warm_up(&mut self, settings: &Settings) {
        match settings.ai.provider {
            AiProvider::LlamaCpp => {
                if let Err(error) = llama_cpp::warm_up(settings) {
                    warn!(%error, "failed to start llama.cpp warmup");
                }
            }
            AiProvider::Ollama | AiProvider::Openai | AiProvider::Anthropic => {}
        }
    }

    pub(crate) async fn summarise(
        &mut self,
        settings: &Settings,
        transcript: &str,
    ) -> SummarisationOutcome {
        match settings.ai.provider {
            AiProvider::LlamaCpp => match llama_cpp::summarise(settings, transcript).await {
                Ok(markdown) => {
                    SummarisationOutcome::Ready(builtin::notes_from_markdown(markdown, transcript))
                }
                Err(error) => {
                    warn!(%error, "llama.cpp summarisation failed; writing transcript-only notes");
                    SummarisationOutcome::Ready(builtin::summarise_with_reason(
                        transcript,
                        format!("llama.cpp summarisation unavailable: {error}"),
                    ))
                }
            },
            AiProvider::Ollama => match ollama::summarise(settings, transcript).await {
                Ok(markdown) => {
                    SummarisationOutcome::Ready(builtin::notes_from_markdown(markdown, transcript))
                }
                Err(error) => {
                    warn!(%error, "ollama summarisation failed; writing transcript-only notes");
                    SummarisationOutcome::Ready(builtin::summarise_with_reason(
                        transcript,
                        format!("Ollama summarisation unavailable: {error}"),
                    ))
                }
            },
            AiProvider::Openai | AiProvider::Anthropic => {
                warn!(provider = ?settings.ai.provider, "hosted AI provider is not implemented yet");
                SummarisationOutcome::Ready(builtin::summarise_with_reason(
                    transcript,
                    "The configured hosted AI provider is not implemented yet.",
                ))
            }
        }
    }
}

pub(crate) enum SummarisationOutcome {
    Ready(MeetingNotes),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_notes_include_reason_without_transcript() {
        let notes = builtin::summarise_with_reason(
            "Alex: We agreed to keep the manual transcript flow for the MVP.",
            "llama.cpp summarisation unavailable",
        );

        assert!(notes.markdown.contains("## Warning"));
        assert!(notes
            .markdown
            .contains("Transcript draft remains available separately"));
        assert!(!notes.markdown.contains("## Transcript"));
        assert!(!notes.markdown.contains("Alex: We agreed"));
    }

    #[test]
    fn markdown_notes_use_first_non_heading_line_as_summary() {
        let notes = builtin::notes_from_markdown(
            "# Meeting Notes\n\nA short summary line.\n\n## Action Items\n\n- Follow up".to_owned(),
            "Alex: Follow up on the manual transcript flow.",
        );

        assert_eq!(notes.summary, "A short summary line.");
        assert!(notes.markdown.contains("- Follow up"));
        assert!(!notes.markdown.contains("## Transcript"));
        assert!(!notes
            .markdown
            .contains("Alex: Follow up on the manual transcript flow."));
    }

    #[test]
    fn markdown_notes_strip_generated_transcript_breakdown() {
        let notes = builtin::notes_from_markdown(
            "# Meeting Notes\n\n## Summary\n\nGrounded summary.\n\n**Transcript Breakdown**\n\nGenerated transcript-like content.".to_owned(),
            "Actual transcript text.",
        );

        assert!(!notes.markdown.contains("Transcript Breakdown"));
        assert!(!notes.markdown.contains("Generated transcript-like content"));
        assert!(!notes.markdown.contains("## Transcript"));
        assert!(!notes.markdown.contains("Actual transcript text."));
    }
}
