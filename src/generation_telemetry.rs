use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationSource {
    OperatorChat,
    BackgroundChat,
    ScheduledChat,
    SelfDirective,
    Heartbeat,
    PluginEvent,
    Orientation,
    Journal,
    Dream,
    Social,
    Reasoning,
    PersonaTrajectory,
    PersonaSnapshot,
    ConversationSummary,
    ConversationTitle,
    Vision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationOutcome {
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GenerationMetricSample {
    pub text: String,
    pub logprob: Option<f32>,
    pub entropy: Option<f32>,
    pub novelty: f32,
}

#[derive(Debug, Clone)]
pub struct ProviderToken {
    pub text: String,
    pub logprob: Option<f32>,
    pub entropy: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GenerationEvent {
    Started {
        generation_id: String,
        source: GenerationSource,
        conversation_id: Option<String>,
    },
    Metrics {
        generation_id: String,
        source: GenerationSource,
        conversation_id: Option<String>,
        samples: Vec<GenerationMetricSample>,
    },
    Finished {
        generation_id: String,
        source: GenerationSource,
        conversation_id: Option<String>,
        outcome: GenerationOutcome,
    },
}

pub type GenerationEventSink = Arc<dyn Fn(GenerationEvent) + Send + Sync>;

#[derive(Clone)]
pub struct GenerationObserver {
    source: GenerationSource,
    conversation_id: Option<String>,
    sink: GenerationEventSink,
}

impl fmt::Debug for GenerationObserver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GenerationObserver")
            .field("source", &self.source)
            .field("conversation_id", &self.conversation_id)
            .finish_non_exhaustive()
    }
}

impl GenerationObserver {
    pub fn new(
        source: GenerationSource,
        conversation_id: Option<String>,
        sink: GenerationEventSink,
    ) -> Self {
        Self {
            source,
            conversation_id,
            sink,
        }
    }

    pub fn with_source(&self, source: GenerationSource) -> Self {
        Self::new(source, self.conversation_id.clone(), Arc::clone(&self.sink))
    }

    pub fn with_conversation(&self, conversation_id: Option<String>) -> Self {
        Self::new(self.source, conversation_id, Arc::clone(&self.sink))
    }

    pub fn start(&self) -> GenerationSession {
        let generation_id = Uuid::new_v4().to_string();
        (self.sink)(GenerationEvent::Started {
            generation_id: generation_id.clone(),
            source: self.source,
            conversation_id: self.conversation_id.clone(),
        });
        GenerationSession {
            generation_id,
            source: self.source,
            conversation_id: self.conversation_id.clone(),
            sink: Arc::clone(&self.sink),
            finished: false,
        }
    }

    pub fn observe_complete_text(&self, text: &str) {
        let mut session = self.start();
        session.finish_with_text(text);
    }
}

impl GenerationSession {
    pub fn finish_with_text(&mut self, text: &str) {
        let mut tracker = TokenNoveltyTracker::default();
        let mut samples = tracker.ingest_text_fragment(text);
        samples.extend(tracker.finish_pending());
        self.emit_samples(samples);
        self.finish(GenerationOutcome::Completed);
    }
}

pub struct GenerationSession {
    generation_id: String,
    source: GenerationSource,
    conversation_id: Option<String>,
    sink: GenerationEventSink,
    finished: bool,
}

impl GenerationSession {
    pub fn emit_samples(&self, samples: Vec<GenerationMetricSample>) {
        if samples.is_empty() {
            return;
        }
        (self.sink)(GenerationEvent::Metrics {
            generation_id: self.generation_id.clone(),
            source: self.source,
            conversation_id: self.conversation_id.clone(),
            samples,
        });
    }

    pub fn finish(&mut self, outcome: GenerationOutcome) {
        if self.finished {
            return;
        }
        self.finished = true;
        (self.sink)(GenerationEvent::Finished {
            generation_id: self.generation_id.clone(),
            source: self.source,
            conversation_id: self.conversation_id.clone(),
            outcome,
        });
    }
}

impl Drop for GenerationSession {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(GenerationOutcome::Failed);
        }
    }
}

#[derive(Debug, Default)]
pub struct TokenNoveltyTracker {
    pending_fragment: String,
    total_tokens: u64,
    token_counts: HashMap<String, u64>,
    bigram_counts: HashMap<String, u64>,
    previous_token: Option<String>,
}

impl TokenNoveltyTracker {
    pub fn ingest_text_fragment(&mut self, fragment: &str) -> Vec<GenerationMetricSample> {
        self.tokenize_fragment(fragment)
            .into_iter()
            .map(|text| {
                self.score_token(ProviderToken {
                    text,
                    logprob: None,
                    entropy: None,
                })
            })
            .collect()
    }

    pub fn ingest_provider_tokens(
        &mut self,
        tokens: Vec<ProviderToken>,
    ) -> Vec<GenerationMetricSample> {
        tokens
            .into_iter()
            .map(|token| self.score_token(token))
            .collect()
    }

    pub fn finish_pending(&mut self) -> Vec<GenerationMetricSample> {
        if self.pending_fragment.trim().is_empty() {
            self.pending_fragment.clear();
            return Vec::new();
        }

        let text = std::mem::take(&mut self.pending_fragment);
        vec![self.score_token(ProviderToken {
            text,
            logprob: None,
            entropy: None,
        })]
    }

    fn tokenize_fragment(&mut self, fragment: &str) -> Vec<String> {
        let mut combined = String::new();
        combined.push_str(&self.pending_fragment);
        combined.push_str(fragment);
        self.pending_fragment.clear();

        let mut tokens = Vec::new();
        let mut current = String::new();
        for ch in combined.chars() {
            if is_metric_word_char(ch) {
                current.push(ch);
                continue;
            }
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            if !ch.is_whitespace() {
                tokens.push(ch.to_string());
            }
        }

        if !current.is_empty() {
            if combined.chars().last().is_some_and(is_metric_word_char) {
                self.pending_fragment = current;
            } else {
                tokens.push(current);
            }
        }
        tokens
    }

    fn score_token(&mut self, token: ProviderToken) -> GenerationMetricSample {
        let normalized = normalize_metric_token(&token.text);
        let seen_count = self.token_counts.get(&normalized).copied().unwrap_or(0) as f32;
        let total = self.total_tokens as f32;
        let vocab = self.token_counts.len() as f32 + 1.0;
        let smoothed_probability = (seen_count + 1.0) / (total + vocab);
        let frequency_novelty = (-smoothed_probability.ln() / 5.5).clamp(0.0, 1.0);
        let bigram_novelty = if let Some(previous) = self.previous_token.as_ref() {
            let count = self
                .bigram_counts
                .get(&bigram_key(previous, &normalized))
                .copied()
                .unwrap_or(0) as f32;
            if count == 0.0 {
                1.0
            } else {
                (1.0 / (count + 1.0)).clamp(0.0, 1.0)
            }
        } else {
            0.55
        };
        let surprisal_score = token
            .logprob
            .map(|value| (-value / 5.0).clamp(0.0, 1.0))
            .unwrap_or(frequency_novelty);
        let entropy_score = token
            .entropy
            .map(|value| (value / 1.75).clamp(0.0, 1.0))
            .unwrap_or(0.0);
        let repeat_penalty = self
            .previous_token
            .as_ref()
            .filter(|previous| *previous == &normalized)
            .map(|_| 0.28)
            .unwrap_or(0.0);
        let novelty = (0.5 * frequency_novelty
            + 0.3 * surprisal_score
            + 0.15 * bigram_novelty
            + 0.05 * entropy_score
            - repeat_penalty)
            .clamp(0.0, 1.25);

        self.total_tokens += 1;
        *self.token_counts.entry(normalized.clone()).or_default() += 1;
        if let Some(previous) = self.previous_token.replace(normalized.clone()) {
            *self
                .bigram_counts
                .entry(bigram_key(&previous, &normalized))
                .or_default() += 1;
        }

        GenerationMetricSample {
            text: token.text,
            logprob: token.logprob,
            entropy: token.entropy,
            novelty,
        }
    }
}

fn is_metric_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '\'' | '-')
}

fn normalize_metric_token(token: &str) -> String {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.chars().any(char::is_alphanumeric) {
        trimmed.to_ascii_lowercase()
    } else {
        trimmed.to_string()
    }
}

fn bigram_key(previous: &str, current: &str) -> String {
    format!("{previous}\u{1f}{current}")
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn complete_text_emits_one_generation_with_synthetic_metrics() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&events);
        let sink: GenerationEventSink = Arc::new(move |event| {
            captured.lock().unwrap().push(event);
        });
        let observer = GenerationObserver::new(GenerationSource::Dream, None, sink);

        observer.observe_complete_text("A strange new pattern appears.");

        let events = events.lock().unwrap();
        assert!(matches!(
            events.first(),
            Some(GenerationEvent::Started { .. })
        ));
        assert!(
            matches!(events.get(1), Some(GenerationEvent::Metrics { samples, .. }) if !samples.is_empty())
        );
        assert!(matches!(
            events.last(),
            Some(GenerationEvent::Finished {
                outcome: GenerationOutcome::Completed,
                ..
            })
        ));
    }

    #[test]
    fn provider_logprob_is_preserved_in_metric_sample() {
        let mut tracker = TokenNoveltyTracker::default();
        let samples = tracker.ingest_provider_tokens(vec![ProviderToken {
            text: "rare".to_string(),
            logprob: Some(-3.25),
            entropy: Some(0.8),
        }]);
        assert_eq!(samples[0].logprob, Some(-3.25));
        assert_eq!(samples[0].entropy, Some(0.8));
    }
}
