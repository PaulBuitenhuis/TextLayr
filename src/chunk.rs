//! Group transcribed words into caption chunks (one popup per chunk).

use crate::transcribe::Word;

/// Words shown together in one popup, plus the time window during which the
/// popup is on screen.
#[derive(Debug, Clone)]
pub struct Chunk {
    pub words: Vec<Word>,
    /// When the popup appears (first word's start).
    pub start_secs: f64,
    /// When the popup disappears (last word's end, extended slightly to bridge
    /// micro-gaps to the next chunk).
    pub end_secs: f64,
}

/// How long a highlight window should last at minimum, so whisper's jittery
/// word starts can't produce a two-frame flash of the next word.
pub const MIN_STATE_SECS: f64 = 0.10;

impl Chunk {
    /// Split the chunk's time span into per-word highlight windows.
    ///
    /// Word starts come from whisper (DTW) and are close, but not perfect:
    /// back-to-back words sometimes start 20 ms apart. Windows are forced
    /// monotonic with a minimum step, clamped so they always tile the chunk's
    /// span exactly — no flashes, no overlaps, no gaps inside a popup.
    ///
    /// Too-fast chunks (span < n * minimum) fall back to an even split.
    pub fn state_windows(&self) -> Vec<(f64, f64)> {
        let n = self.words.len();
        let start = self.start_secs;
        let end = self.end_secs.max(start);
        let span = end - start;

        if n == 0 {
            return Vec::new();
        }
        if span <= 0.0 {
            // Degenerate chunk: give every word a sliver so the popup still
            // appears for at least a frame or two.
            return vec![(start, start + 0.01); n];
        }

        let step = MIN_STATE_SECS.min(span / n as f64);
        let mut bounds = Vec::with_capacity(n + 1);
        bounds.push(start);
        for k in 1..n {
            let raw = self.words[k].start_secs;
            let lo = bounds[k - 1] + step;
            let hi = (end - (n - k) as f64 * step).max(lo);
            bounds.push(raw.clamp(lo, hi));
        }
        bounds.push(end);

        bounds
            .windows(2)
            .map(|w| (w[0], w[1]))
            .collect()
    }

    /// Text of the chunk as a single string (used for preview/debug output).
    #[allow(dead_code)] // exercised by tests; the review UI uses it in M3
    pub fn text(&self) -> String {
        self.words
            .iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Build a chunk from *edited* caption text (DESIGN's editing rule):
    /// words are whitespace-split and re-timed **evenly within the chunk's
    /// original time span** — editing changes text, never when the popup is
    /// on screen. Empty text yields a wordless chunk (rendering skips it).
    pub fn from_edited_text(text: &str, start_secs: f64, end_secs: f64) -> Chunk {
        let texts: Vec<&str> = text.split_whitespace().collect();
        let n = texts.len();
        let span = (end_secs - start_secs).max(0.0);
        let step = if n > 0 { span / n as f64 } else { 0.0 };
        let words = texts
            .iter()
            .enumerate()
            .map(|(i, t)| Word {
                text: (*t).to_string(),
                start_secs: start_secs + step * i as f64,
                end_secs: start_secs + step * (i + 1) as f64,
            })
            .collect();
        Chunk {
            words,
            start_secs,
            end_secs,
        }
    }
}

/// How long the last word of a chunk stays on screen past its own end, to
/// avoid a one-frame flicker before the next popup. Capped by the gap to the
/// next chunk so two popups are never visible at once.
const GAP_BRIDGE_SECS: f64 = 0.25;

/// True if the word ends a sentence or a clause worth breaking on.
fn is_break(word: &str) -> bool {
    word.ends_with(['.', '!', '?', ';', ':', '…'])
        || word.ends_with(['.', '!', '?', ';', ':', '…', '"', '\''])
}

/// Group words into chunks of `words_per` (clamped to 2..=6), preferring to
/// break at sentence/clause punctuation rather than cutting mid-phrase.
///
/// A short trailing group is merged back into the previous chunk so the last
/// popup is not a lonely one- or two-word stub.
pub fn chunk_words(words: &[Word], words_per: usize) -> Vec<Chunk> {
    if words.is_empty() {
        return Vec::new();
    }
    let target = words_per.clamp(2, 6);
    let min_words = target.min(3);

    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();

    for (i, w) in words.iter().enumerate() {
        current.push(i);
        let text = w.text.trim();
        let hard_full = current.len() >= target;
        let soft_break = current.len() >= min_words && is_break(text);
        if hard_full || soft_break {
            groups.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        // Fold a short trailing stub back into the previous chunk (so the last
        // popup isn't a lonely one- or two-word group), but never inflate a
        // chunk by more than one word: uniform popups matter more.
        match groups.last_mut() {
            Some(last)
                if current.len() < min_words && last.len() + current.len() <= target + 1 =>
            {
                last.append(&mut current);
            }
            _ => groups.push(std::mem::take(&mut current)),
        }
    }

    let mut chunks = Vec::with_capacity(groups.len());
    for (gi, g) in groups.iter().enumerate() {
        let first = &words[g[0]];
        let last = &words[g[g.len() - 1]];
        let mut chunk = Chunk {
            words: g.iter().map(|&i| words[i].clone()).collect(),
            start_secs: first.start_secs,
            end_secs: last.end_secs,
        };

        // Bridge short gaps to the next popup; never overlap it.
        if let Some(next) = groups.get(gi + 1) {
            let next_start = words[next[0]].start_secs;
            let gap = next_start - chunk.end_secs;
            if gap > 0.0 {
                chunk.end_secs += gap.min(GAP_BRIDGE_SECS);
            }
        }
        chunks.push(chunk);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str, start: f64, end: f64) -> Word {
        Word {
            text: text.to_string(),
            start_secs: start,
            end_secs: end,
        }
    }

    #[test]
    fn groups_by_target_size() {
        let words: Vec<Word> = (0..9).map(|i| w("x", i as f64, i as f64 + 0.5)).collect();
        let chunks = chunk_words(&words, 3);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|c| c.words.len() == 3));
    }

    #[test]
    fn prefers_punctuation_breaks() {
        // 4 words, break after the period, target 6 -> two chunks of 2? No:
        // min_words=3, so the soft break only fires at 3+ words.
        let words = vec![
            w("one", 0.0, 0.5),
            w("two,", 0.5, 1.0),
            w("three.", 1.0, 1.5),
            w("four", 1.5, 2.0),
            w("five", 2.0, 2.5),
            w("six", 2.5, 3.0),
        ];
        let chunks = chunk_words(&words, 6);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text(), "one two, three.");
        assert_eq!(chunks[1].text(), "four five six");
    }

    #[test]
    fn folds_short_tail_into_previous() {
        // 10 words at target 3 -> 3, 3, then a 1-word stub folded into the
        // previous chunk (3 + 1 <= 3 + 1), giving 3, 3, 4.
        let words: Vec<Word> = (0..10).map(|i| w("x", i as f64, i as f64 + 0.5)).collect();
        let chunks = chunk_words(&words, 3);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].words.len(), 3);
        assert_eq!(chunks[1].words.len(), 3);
        assert_eq!(chunks[2].words.len(), 4);
    }

    #[test]
    fn does_not_inflate_a_chunk_past_one_extra_word() {
        // 8 words at target 3 -> tail of 2 is too fat to fold (3 + 2 > 3 + 1),
        // so it ships as its own 2-word popup rather than a 5-word one.
        let words: Vec<Word> = (0..8).map(|i| w("x", i as f64, i as f64 + 0.5)).collect();
        let chunks = chunk_words(&words, 3);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[2].words.len(), 2);
    }

    #[test]
    fn bridges_small_gaps_but_never_overlaps() {
        let words = vec![
            w("a", 0.0, 0.5),
            w("b", 0.6, 1.1),
            w("c", 2.0, 2.5),
            w("d", 2.6, 3.1),
        ];
        let chunks = chunk_words(&words, 2);
        assert_eq!(chunks.len(), 2);
        // 0.9s gap: only 0.25s bridged, so the popup leaves before the next
        // one arrives.
        assert!((chunks[0].end_secs - 1.35).abs() < 1e-9);
        assert!(chunks[0].end_secs < chunks[1].start_secs);
    }

    #[test]
    fn windows_tile_the_chunk_with_a_minimum_step() {
        // Whisper jitter: the last two words start 20 ms apart. The window for
        // word 1 must not collapse to a flash.
        let chunk = Chunk {
            words: vec![w("one", 0.0, 0.2), w("two", 0.6, 0.7), w("three", 0.62, 1.0)],
            start_secs: 0.0,
            end_secs: 1.0,
        };
        let win = chunk.state_windows();
        assert_eq!(win.len(), 3);
        // Contiguous, no gaps or overlaps.
        assert_eq!(win[0].0, 0.0);
        assert_eq!(win[1].0, win[0].1);
        assert_eq!(win[2].0, win[1].1);
        assert_eq!(win[2].1, 1.0);
        // Every window respects the minimum step and tracks the word starts.
        for (s, e) in &win {
            assert!(e - s >= MIN_STATE_SECS - 1e-9);
        }
        assert!((win[0].1 - 0.6).abs() < 1e-9); // word 2 starts on time
        assert!(win[2].0 >= 0.7); // word 3 pushed clear of word 2
    }

    #[test]
    fn fast_chunks_fall_back_to_an_even_split() {
        // 4 words squeezed into 0.3 s: the minimum step can't fit, so windows
        // divide the span evenly instead of running past the chunk end.
        let words: Vec<Word> = (0..4)
            .map(|i| w("x", 0.05 * i as f64, 0.05 * i as f64 + 0.04))
            .collect();
        let chunks = chunk_words(&words, 4);
        let win = chunks[0].state_windows();
        assert_eq!(win.len(), 4);
        for (s, e) in &win {
            assert!(e > s);
            assert!(*s >= chunks[0].start_secs - 1e-9);
            assert!(*e <= chunks[0].end_secs + 1e-9);
        }
        assert_eq!(win[3].1, chunks[0].end_secs);
    }

    #[test]
    fn edited_text_is_retimed_evenly_within_the_original_span() {
        // The user swapped 2 words for 4: same on-screen span, even split.
        let c = Chunk::from_edited_text("one two three four", 10.0, 12.0);
        assert_eq!(c.text(), "one two three four");
        assert_eq!(c.start_secs, 10.0);
        assert_eq!(c.end_secs, 12.0);
        assert_eq!(c.words.len(), 4);
        for (i, w) in c.words.iter().enumerate() {
            let expect_start = 10.0 + 0.5 * i as f64;
            assert!((w.start_secs - expect_start).abs() < 1e-9);
            assert!((w.end_secs - (expect_start + 0.5)).abs() < 1e-9);
        }
        // The windows the renderer will show tile the span exactly.
        let win = c.state_windows();
        assert_eq!(win[0].0, 10.0);
        assert_eq!(win[3].1, 12.0);
    }

    #[test]
    fn edited_text_collapses_whitespace_and_handles_empty() {
        let c = Chunk::from_edited_text("  hello   world  ", 0.0, 1.0);
        assert_eq!(c.words.len(), 2);
        assert_eq!(c.text(), "hello world");

        let empty = Chunk::from_edited_text("   ", 0.0, 1.0);
        assert!(empty.words.is_empty());
        assert!(empty.state_windows().is_empty());
    }
}
