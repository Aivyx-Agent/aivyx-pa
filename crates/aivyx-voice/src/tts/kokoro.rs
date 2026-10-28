//! Kokoro TTS backend — the permissive replacement for Piper
//! (Chapter Timbre, `docs/TIMBRE.md`).
//!
//! TB.1 spike: a `TtsEngine` impl that runs the **Kokoro-82M**
//! acoustic model (Apache-2.0 weights) via `ort` (ONNX Runtime,
//! Apache/MIT), phonemizing with `voice-g2p` (MIT,
//! dictionary-based — **no espeak-ng linked**, so nothing GPL
//! enters the binary). This is the whole point of the chapter:
//! the voice stack becomes fully source-available-clean.
//!
//! ## Pipeline
//!
//! ```text
//! text → voice_g2p::english_to_phonemes → IPA phonemes
//!      → vocab (char→id)                 → input_ids
//!      → ort.run(input_ids, style, speed)→ f32 PCM @ 24 kHz
//! ```
//!
//! The Kokoro ONNX inputs are `[0, t1..tN, 0]` token ids (shape
//! `[1, N+2]`), a `[1, 256]` style vector selected from the voices
//! archive by phoneme-token count, and a `speed` scalar (int32 or
//! float32 — autodetected from the model's declared input type).
//! The first output tensor is the waveform.
//!
//! ## Files an operator supplies (model dir)
//!
//! - a `*.onnx` Kokoro model,
//! - `voices-v1.0.bin` (an npz of `<voice>.npy` `[N, 256]` f32),
//! - optionally `config.json` (the `"vocab"` map; we fall back to
//!   the embedded [`hardcoded_vocab`] when absent).
//!
//! ## Threading
//!
//! ONNX inference is sync + CPU-heavy. The `Session` lives behind a
//! `Mutex` (serial use, matching the push-to-talk loop); the
//! channel layer should wrap [`KokoroEngine::synthesize`] in
//! `tokio::task::spawn_blocking` when integrating (same posture as
//! Piper / whisper-rs).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use ndarray::Array2;
use ort::execution_providers::CPUExecutionProvider;
use ort::inputs;
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::TensorRef;

use crate::tts::{TtsAudio, TtsConfig, TtsEngine, TtsError};

/// Kokoro style-vector dimension.
pub const STYLE_DIM: usize = 256;

/// Kokoro acoustic-model output sample rate.
pub const SAMPLE_RATE: u32 = 24_000;

/// Max phoneme tokens the model accepts per inference (before the
/// two padding tokens). Longer inputs are split at punctuation.
pub const MAX_PHONEME_LEN: usize = 510;

/// Operator-supplied Kokoro configuration.
#[derive(Debug, Clone)]
pub struct KokoroTtsConfig {
    /// Directory holding the `.onnx` model, `voices-*.bin`, and
    /// optionally `config.json`.
    pub model_dir: PathBuf,
    /// Voice name (an entry in the voices archive, e.g.
    /// `af_heart`).
    pub voice_name: String,
    /// Speaking rate multiplier (1.0 = normal).
    pub speed: f32,
}

impl KokoroTtsConfig {
    pub fn new(model_dir: impl Into<PathBuf>) -> Self {
        KokoroTtsConfig {
            model_dir: model_dir.into(),
            voice_name: "af_heart".to_string(),
            speed: 1.0,
        }
    }

    pub fn with_voice(mut self, voice: impl Into<String>) -> Self {
        self.voice_name = voice.into();
        self
    }

    pub fn with_speed(mut self, speed: f32) -> Self {
        self.speed = speed;
        self
    }
}

/// Build a [`KokoroTtsConfig`] from the engine-neutral
/// [`TtsConfig`]. `model_dir` is required; `voice_name` and
/// `speed` fall back to the engine defaults (`af_heart`, `1.0`).
pub fn config_from_generic(generic: &TtsConfig) -> Result<KokoroTtsConfig, TtsError> {
    let model_dir = generic.model_dir.clone().ok_or_else(|| {
        TtsError::ModelLoad(
            "[voice.tts] model_dir missing — set the absolute path to the Kokoro model \
             directory (the folder holding the .onnx model + voices-*.bin)"
                .to_string(),
        )
    })?;
    let mut cfg = KokoroTtsConfig::new(model_dir);
    if let Some(voice) = &generic.voice_name {
        cfg = cfg.with_voice(voice.clone());
    }
    if let Some(speed) = generic.speed {
        cfg = cfg.with_speed(speed);
    }
    Ok(cfg)
}

/// `TtsEngine` backed by Kokoro-82M over ONNX Runtime.
pub struct KokoroEngine {
    session: Mutex<Session>,
    voices: VoiceStore,
    vocab: HashMap<char, i64>,
    /// The model's token input name — "input_ids" or "tokens".
    tokens_input_name: String,
    /// True when the `speed` input is int32 (modern exports), else
    /// float32.
    speed_is_int32: bool,
    voice_name: String,
    speed: f32,
}

impl KokoroEngine {
    /// Load the model + voices + vocab from a [`KokoroTtsConfig`].
    pub fn new(config: KokoroTtsConfig) -> Result<Self, TtsError> {
        let onnx_path = find_onnx_file(&config.model_dir)?;
        let session = init_session(&onnx_path)?;

        let tokens_input_name = detect_tokens_input(&session);
        let speed_is_int32 = detect_speed_type(&session);

        let voices_path = find_voices_file(&config.model_dir)?;
        let voices = VoiceStore::load(&voices_path)?;
        if !voices.contains(&config.voice_name) {
            return Err(TtsError::ModelLoad(format!(
                "voice {:?} not in {} (available: {})",
                config.voice_name,
                voices_path.display(),
                voices.list_voices().join(", ")
            )));
        }

        let config_json = config.model_dir.join("config.json");
        let vocab = if config_json.exists() {
            load_vocab(&config_json)?
        } else {
            hardcoded_vocab()
        };

        Ok(KokoroEngine {
            session: Mutex::new(session),
            voices,
            vocab,
            tokens_input_name,
            speed_is_int32,
            voice_name: config.voice_name,
            speed: config.speed,
        })
    }

    /// Phonemize `text` (via `voice-g2p`) and map to Kokoro token
    /// ids through the vocab. Characters absent from the vocab are
    /// dropped (matching the reference behaviour). Pure except for
    /// the G2P call.
    fn text_to_token_ids(&self, text: &str) -> Result<Vec<i64>, TtsError> {
        let phonemes = voice_g2p::english_to_phonemes(text)
            .map_err(|e| TtsError::Synthesis(format!("g2p: {e}")))?;
        Ok(map_phonemes_to_ids(&phonemes, &self.vocab))
    }

    /// Run ONNX inference for one token chunk → PCM samples.
    fn synthesize_chunk(&self, tokens: &[i64]) -> Result<Vec<f32>, TtsError> {
        // Style vector is indexed by token count (prosody cue).
        let style = self.voices.get_style(&self.voice_name, tokens.len())?;

        // Tokens padded with a leading + trailing 0: [0, t.., 0].
        let seq_len = tokens.len() + 2;
        let mut padded = vec![0i64; seq_len];
        padded[1..seq_len - 1].copy_from_slice(tokens);
        let tokens_arr = Array2::from_shape_vec((1, seq_len), padded)
            .map_err(|e| TtsError::Synthesis(format!("tokens shape: {e}")))?;

        let style_view = ndarray::ArrayView2::from_shape((1, STYLE_DIM), style.as_slice())
            .map_err(|e| TtsError::Synthesis(format!("style shape: {e}")))?;

        let mut session = self
            .session
            .lock()
            .map_err(|_| TtsError::Synthesis("kokoro session mutex poisoned".to_string()))?;

        let outputs = if self.speed_is_int32 {
            let speed_arr = ndarray::arr1(&[self.speed as i32]);
            let inputs = inputs![
                self.tokens_input_name.as_str() => TensorRef::from_array_view(tokens_arr.view())
                    .map_err(|e| TtsError::Synthesis(format!("tokens tensor: {e}")))?,
                "style" => TensorRef::from_array_view(style_view)
                    .map_err(|e| TtsError::Synthesis(format!("style tensor: {e}")))?,
                "speed" => TensorRef::from_array_view(speed_arr.view())
                    .map_err(|e| TtsError::Synthesis(format!("speed tensor: {e}")))?,
            ];
            session
                .run(inputs)
                .map_err(|e| TtsError::Synthesis(format!("inference: {e}")))?
        } else {
            let speed_arr = ndarray::arr1(&[self.speed]);
            let inputs = inputs![
                self.tokens_input_name.as_str() => TensorRef::from_array_view(tokens_arr.view())
                    .map_err(|e| TtsError::Synthesis(format!("tokens tensor: {e}")))?,
                "style" => TensorRef::from_array_view(style_view)
                    .map_err(|e| TtsError::Synthesis(format!("style tensor: {e}")))?,
                "speed" => TensorRef::from_array_view(speed_arr.view())
                    .map_err(|e| TtsError::Synthesis(format!("speed tensor: {e}")))?,
            ];
            session
                .run(inputs)
                .map_err(|e| TtsError::Synthesis(format!("inference: {e}")))?
        };

        let (_, first) = outputs
            .iter()
            .next()
            .ok_or_else(|| TtsError::Synthesis("model produced no output".to_string()))?;
        let waveform = first
            .try_extract_array::<f32>()
            .map_err(|e| TtsError::Synthesis(format!("extract waveform: {e}")))?;
        Ok(waveform.as_slice().unwrap_or(&[]).to_vec())
    }

    /// Available voice names (sorted).
    pub fn voices(&self) -> Vec<&str> {
        self.voices.list_voices()
    }
}

#[async_trait]
impl TtsEngine for KokoroEngine {
    async fn synthesize(&self, text: &str) -> Result<TtsAudio, TtsError> {
        if text.trim().is_empty() {
            return Err(TtsError::Input("empty text — TTS skipped".to_string()));
        }
        let ids = self.text_to_token_ids(text)?;
        if ids.is_empty() {
            return Err(TtsError::Input(format!(
                "no phoneme tokens produced for {text:?}"
            )));
        }

        let mut samples = Vec::new();
        for chunk in split_chunks(&ids) {
            let audio = self.synthesize_chunk(&chunk)?;
            samples.extend_from_slice(&audio);
        }
        Ok(TtsAudio::new(samples, SAMPLE_RATE))
    }

    fn native_sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }
}

// ---------------------------------------------------------------------------
// Pure substrate — phoneme mapping, chunking, vocab, voices parsing.
// Directly unit-testable; no ONNX, no IO (except VoiceStore::load).
// ---------------------------------------------------------------------------

/// Map a Kokoro phoneme string to token ids, dropping any char not
/// in the vocab. Pure.
pub fn map_phonemes_to_ids(phonemes: &str, vocab: &HashMap<char, i64>) -> Vec<i64> {
    phonemes.chars().filter_map(|c| vocab.get(&c).copied()).collect()
}

/// Punctuation token ids in the canonical vocab (`; : , . ! ?`),
/// used as preferred split points for over-long sequences.
const PUNCT_IDS: &[i64] = &[1, 2, 3, 4, 5, 6];

/// Split token ids into chunks of at most [`MAX_PHONEME_LEN`],
/// preferring to break just after punctuation. Pure.
pub fn split_chunks(ids: &[i64]) -> Vec<Vec<i64>> {
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < ids.len() {
        let end = (start + MAX_PHONEME_LEN).min(ids.len());
        if end == ids.len() {
            chunks.push(ids[start..end].to_vec());
            break;
        }
        let split = ids[start..end]
            .iter()
            .enumerate()
            .rev()
            .find(|&(_, &id)| PUNCT_IDS.contains(&id))
            .map(|(i, _)| start + i + 1)
            .unwrap_or(end);
        chunks.push(ids[start..split].to_vec());
        start = split;
    }
    chunks
}

/// Load the Kokoro `char → id` vocab from a `config.json`'s
/// `"vocab"` object.
pub fn load_vocab(config_path: &Path) -> Result<HashMap<char, i64>, TtsError> {
    let content = std::fs::read_to_string(config_path)
        .map_err(|e| TtsError::ModelLoad(format!("read config.json: {e}")))?;
    let json: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| TtsError::ModelLoad(format!("parse config.json: {e}")))?;
    let obj = json
        .get("vocab")
        .and_then(|v| v.as_object())
        .ok_or_else(|| TtsError::ModelLoad("config.json missing object 'vocab'".to_string()))?;
    let mut map = HashMap::new();
    for (k, v) in obj {
        if let (Some(ch), Some(id)) = (k.chars().next(), v.as_i64()) {
            map.insert(ch, id);
        }
    }
    Ok(map)
}

/// The canonical Kokoro vocabulary, embedded for when no
/// `config.json` ships beside the model. Stable across Kokoro
/// model versions.
pub fn hardcoded_vocab() -> HashMap<char, i64> {
    let entries: &[(char, i64)] = &[
        (';', 1), (':', 2), (',', 3), ('.', 4), ('!', 5), ('?', 6),
        ('—', 9), ('…', 10), ('"', 11), ('(', 12), (')', 13),
        ('\u{201c}', 14), ('\u{201d}', 15), (' ', 16), ('\u{0303}', 17),
        ('ʣ', 18), ('ʥ', 19), ('ʦ', 20), ('ʨ', 21), ('ᵝ', 22), ('ꭧ', 23),
        ('A', 24), ('I', 25), ('O', 31), ('Q', 33), ('S', 35), ('T', 36),
        ('W', 39), ('Y', 41), ('ᵊ', 42), ('a', 43), ('b', 44), ('c', 45),
        ('d', 46), ('e', 47), ('f', 48), ('h', 50), ('i', 51), ('j', 52),
        ('k', 53), ('l', 54), ('m', 55), ('n', 56), ('o', 57), ('p', 58),
        ('q', 59), ('r', 60), ('s', 61), ('t', 62), ('u', 63), ('v', 64),
        ('w', 65), ('x', 66), ('y', 67), ('z', 68), ('ɑ', 69), ('ɐ', 70),
        ('ɒ', 71), ('æ', 72), ('β', 75), ('ɔ', 76), ('ɕ', 77), ('ç', 78),
        ('ɖ', 80), ('ð', 81), ('ʤ', 82), ('ə', 83), ('ɚ', 85), ('ɛ', 86),
        ('ɜ', 87), ('ɟ', 90), ('ɡ', 92), ('ɥ', 99), ('ɨ', 101), ('ɪ', 102),
        ('ʝ', 103), ('ɯ', 110), ('ɰ', 111), ('ŋ', 112), ('ɳ', 113),
        ('ɲ', 114), ('ɴ', 115), ('ø', 116), ('ɸ', 118), ('θ', 119),
        ('œ', 120), ('ɹ', 123), ('ɾ', 125), ('ɻ', 126), ('ʁ', 128),
        ('ɽ', 129), ('ʂ', 130), ('ʃ', 131), ('ʈ', 132), ('ʧ', 133),
        ('ʊ', 135), ('ʋ', 136), ('ʌ', 138), ('ɣ', 139), ('ɤ', 140),
        ('χ', 142), ('ʎ', 143), ('ʒ', 147), ('ʔ', 148), ('ˈ', 156),
        ('ˌ', 157), ('ː', 158), ('ʰ', 162), ('ʲ', 164), ('↓', 169),
        ('→', 171), ('↗', 172), ('↘', 173), ('ᵻ', 177),
    ];
    entries.iter().copied().collect()
}

/// Loaded per-voice style vectors. Each voice is a list of
/// `[f32; 256]` indexed by phoneme-token count.
struct VoiceStore {
    voices: HashMap<String, Vec<[f32; STYLE_DIM]>>,
}

impl VoiceStore {
    /// Load voices from a `voices-*.bin` npz (zip of `<name>.npy`).
    fn load(path: &Path) -> Result<Self, TtsError> {
        let file = std::fs::File::open(path)
            .map_err(|e| TtsError::ModelLoad(format!("open voices {}: {e}", path.display())))?;
        let mut zip = zip::ZipArchive::new(file)
            .map_err(|e| TtsError::ModelLoad(format!("voices not a zip/npz: {e}")))?;
        let mut voices = HashMap::new();
        for i in 0..zip.len() {
            let mut entry = zip
                .by_index(i)
                .map_err(|e| TtsError::ModelLoad(format!("voices entry {i}: {e}")))?;
            let raw = entry
                .name()
                .map_err(|e| TtsError::ModelLoad(format!("voices entry {i} name: {e}")))?
                .to_string();
            if raw.ends_with('/') {
                continue;
            }
            let name = raw.trim_end_matches(".npy").to_string();
            if name.is_empty() {
                continue;
            }
            let mut data = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut data)
                .map_err(|e| TtsError::ModelLoad(format!("read voice {raw}: {e}")))?;
            voices.insert(name, parse_npy(&data)?);
        }
        if voices.is_empty() {
            return Err(TtsError::ModelLoad("voices archive contained no voices".to_string()));
        }
        Ok(VoiceStore { voices })
    }

    fn contains(&self, voice: &str) -> bool {
        self.voices.contains_key(voice)
    }

    /// Style vector for `voice` at `idx` (clamped into range).
    fn get_style(&self, voice: &str, idx: usize) -> Result<[f32; STYLE_DIM], TtsError> {
        let styles = self
            .voices
            .get(voice)
            .ok_or_else(|| TtsError::Synthesis(format!("voice {voice:?} not loaded")))?;
        let clamped = idx.min(styles.len().saturating_sub(1));
        Ok(styles[clamped])
    }

    fn list_voices(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.voices.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }
}

/// Parse a little-endian float32 numpy `.npy` of shape `[N, 256]`
/// into style vectors. Pure.
fn parse_npy(data: &[u8]) -> Result<Vec<[f32; STYLE_DIM]>, TtsError> {
    if data.len() < 10 || &data[0..6] != b"\x93NUMPY" {
        return Err(TtsError::ModelLoad("voice: bad .npy magic".to_string()));
    }
    let header_len = u16::from_le_bytes([data[8], data[9]]) as usize;
    let offset = 10 + header_len;
    let floats = data
        .get(offset..)
        .ok_or_else(|| TtsError::ModelLoad("voice: truncated .npy header".to_string()))?;
    if floats.len() % 4 != 0 || (floats.len() / 4) % STYLE_DIM != 0 {
        return Err(TtsError::ModelLoad(format!(
            "voice: float count not a multiple of {STYLE_DIM}"
        )));
    }
    let n = floats.len() / 4 / STYLE_DIM;
    let mut out = Vec::with_capacity(n);
    for row in 0..n {
        let mut v = [0f32; STYLE_DIM];
        for (j, slot) in v.iter_mut().enumerate() {
            let o = (row * STYLE_DIM + j) * 4;
            *slot = f32::from_le_bytes([floats[o], floats[o + 1], floats[o + 2], floats[o + 3]]);
        }
        out.push(v);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// ONNX session helpers (model-shape detection + load).
// ---------------------------------------------------------------------------

fn find_onnx_file(dir: &Path) -> Result<PathBuf, TtsError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| TtsError::ModelLoad(format!("read model dir {}: {e}", dir.display())))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("onnx") {
            return Ok(path);
        }
    }
    Err(TtsError::ModelLoad(format!(
        "no .onnx model found in {}",
        dir.display()
    )))
}

fn find_voices_file(dir: &Path) -> Result<PathBuf, TtsError> {
    let preferred = dir.join("voices-v1.0.bin");
    if preferred.exists() {
        return Ok(preferred);
    }
    let entries = std::fs::read_dir(dir)
        .map_err(|e| TtsError::ModelLoad(format!("read model dir {}: {e}", dir.display())))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with("voices") && name.ends_with(".bin") {
            return Ok(path);
        }
    }
    Err(TtsError::ModelLoad(format!(
        "no voices-*.bin found in {}",
        dir.display()
    )))
}

fn init_session(onnx_path: &Path) -> Result<Session, TtsError> {
    Session::builder()
        .map_err(|e| TtsError::ModelLoad(format!("session builder: {e}")))?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| TtsError::ModelLoad(format!("opt level: {e}")))?
        .with_execution_providers(vec![CPUExecutionProvider::default().build()])
        .map_err(|e| TtsError::ModelLoad(format!("providers: {e}")))?
        .commit_from_file(onnx_path)
        .map_err(|e| TtsError::ModelLoad(format!("load {}: {e}", onnx_path.display())))
}

/// The model's token input is named "input_ids" or "tokens".
fn detect_tokens_input(session: &Session) -> String {
    for input in session.inputs() {
        if input.name() == "input_ids" || input.name() == "tokens" {
            return input.name().to_string();
        }
    }
    "input_ids".to_string()
}

/// Modern Kokoro exports take an int32 `speed`; older ones float32.
fn detect_speed_type(session: &Session) -> bool {
    for input in session.inputs() {
        if input.name() == "speed" {
            let ty = format!("{:?}", input.dtype());
            return ty.contains("Int32") || ty.contains("int32");
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocab_has_core_phonemes_and_punct() {
        let v = hardcoded_vocab();
        assert_eq!(v.get(&'.'), Some(&4));
        assert_eq!(v.get(&' '), Some(&16));
        assert_eq!(v.get(&'ˈ'), Some(&156)); // primary stress
        assert_eq!(v.get(&'ɹ'), Some(&123)); // English r
        assert!(v.len() > 100);
    }

    #[test]
    fn phonemes_map_to_ids_and_drop_unknowns() {
        let v = hardcoded_vocab();
        // "hələ" → all in vocab; 'Z' (not a phoneme char) dropped.
        let ids = map_phonemes_to_ids("hˈɛləʊZ", &v);
        // h=50 ˈ=156 ɛ=86 l=54 ə=83 ʊ=135 ; 'Z' dropped
        assert_eq!(ids, vec![50, 156, 86, 54, 83, 135]);
    }

    #[test]
    fn empty_phonemes_yield_no_ids() {
        let v = hardcoded_vocab();
        assert!(map_phonemes_to_ids("", &v).is_empty());
        assert!(map_phonemes_to_ids("ZZZ", &v).is_empty()); // none in vocab
    }

    #[test]
    fn short_sequence_is_one_chunk() {
        let ids: Vec<i64> = (0..10).collect();
        let chunks = split_chunks(&ids);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], ids);
    }

    #[test]
    fn long_sequence_splits_at_punctuation() {
        // 600 tokens with a period (id 4) at index 400 — inside the
        // first MAX_PHONEME_LEN window, so the first chunk must break
        // just after it (index 401) rather than at the hard cap.
        let mut ids = vec![60i64; 600]; // 'r'
        ids[400] = 4; // '.'
        let chunks = split_chunks(&ids);
        assert!(chunks.len() >= 2);
        assert_eq!(chunks[0].len(), 401, "first chunk ends right after the period");
        assert!(chunks[0].len() <= MAX_PHONEME_LEN);
        let total: usize = chunks.iter().map(|c| c.len()).sum();
        assert_eq!(total, 600);
    }

    #[test]
    fn long_sequence_without_punct_breaks_at_cap() {
        let ids = vec![60i64; 1200];
        let chunks = split_chunks(&ids);
        assert_eq!(chunks[0].len(), MAX_PHONEME_LEN);
        let total: usize = chunks.iter().map(|c| c.len()).sum();
        assert_eq!(total, 1200);
    }

    #[test]
    fn parse_npy_roundtrips_two_style_vectors() {
        // Build a minimal .npy: magic + version + header + 2x256 f32.
        let header = "{'descr': '<f4', 'fortran_order': False, 'shape': (2, 256), }";
        // numpy pads the header so total (10 + header_len) % 64 == 0,
        // but our parser only needs header_len to be correct.
        let mut npy = Vec::new();
        npy.extend_from_slice(b"\x93NUMPY");
        npy.push(1); // major
        npy.push(0); // minor
        npy.extend_from_slice(&(header.len() as u16).to_le_bytes());
        npy.extend_from_slice(header.as_bytes());
        for i in 0..(2 * STYLE_DIM) {
            npy.extend_from_slice(&(i as f32).to_le_bytes());
        }
        let styles = parse_npy(&npy).expect("parse");
        assert_eq!(styles.len(), 2);
        assert_eq!(styles[0][0], 0.0);
        assert_eq!(styles[0][1], 1.0);
        assert_eq!(styles[1][0], STYLE_DIM as f32); // row 1, col 0 = 256
    }

    #[test]
    fn parse_npy_rejects_bad_magic() {
        assert!(parse_npy(b"not a numpy file at all").is_err());
    }

    #[test]
    fn config_from_generic_requires_model_dir() {
        let empty = TtsConfig::default();
        assert!(config_from_generic(&empty).is_err());
    }

    #[test]
    fn config_from_generic_maps_fields_with_defaults() {
        let generic = TtsConfig {
            model_dir: Some("/models/kokoro".into()),
            ..Default::default()
        };
        let cfg = config_from_generic(&generic).expect("ok");
        assert_eq!(cfg.model_dir, std::path::PathBuf::from("/models/kokoro"));
        assert_eq!(cfg.voice_name, "af_heart"); // default
        assert_eq!(cfg.speed, 1.0); // default

        let generic = TtsConfig {
            model_dir: Some("/m".into()),
            voice_name: Some("bf_emma".to_string()),
            speed: Some(1.25),
            ..Default::default()
        };
        let cfg = config_from_generic(&generic).expect("ok");
        assert_eq!(cfg.voice_name, "bf_emma");
        assert_eq!(cfg.speed, 1.25);
    }
}
