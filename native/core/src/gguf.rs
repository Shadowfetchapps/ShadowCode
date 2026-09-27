//! Minimal GGUF header reader.
//!
//! Reads only the key/value metadata and tensor names of a GGUF file (never
//! the weights) so the local catalog can report architecture, context window,
//! chat-template support, and a memory estimate from real file metadata
//! instead of file names. Large arrays such as the tokenizer vocabulary are
//! skipped, not loaded. Files written by any GGUF v2/v3 converter (llama.cpp,
//! Ollama, LM Studio) are understood; v1 is rejected because the pinned
//! runtime no longer loads it.
use anyhow::{bail, ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
};

const MAGIC: &[u8; 4] = b"GGUF";
/// Metadata sections larger than this are treated as corrupt rather than read.
const MAX_HEADER_BYTES: u64 = 256 * 1024 * 1024;
const MAX_STRING_BYTES: u64 = 64 * 1024 * 1024;
const MAX_KV: u64 = 4096;
const MAX_TENSORS: u64 = 65536;
/// Strings longer than this are kept only as a prefix. Full default-template
/// identity is recorded separately; a prefix cannot prove absence of tools.
const KEEP_STRING_BYTES: usize = 64 * 1024;
/// Integer arrays up to this length are kept (per-layer hyperparameters);
/// longer ones (token types, merges) are skipped.
const MAX_KEPT_ARRAY: u64 = 4096;

#[derive(Clone, Debug, PartialEq)]
pub enum Scalar {
    U64(u64),
    I64(i64),
    F64(f64),
    Bool(bool),
    Str(String),
    /// Array element count only; contents are skipped.
    Array(u64),
    /// Small integer or boolean array (per-layer values), kept.
    IntArray(Vec<i64>),
}

impl Scalar {
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Scalar::U64(v) => Some(*v),
            Scalar::I64(v) if *v >= 0 => Some(*v as u64),
            Scalar::F64(v) if *v >= 0.0 => Some(*v as u64),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Scalar::Str(v) => Some(v),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Scalar::Bool(v) => Some(*v),
            Scalar::U64(v) => Some(*v != 0),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct GgufHeader {
    pub version: u32,
    pub n_tensors: u64,
    pub metadata: BTreeMap<String, Scalar>,
    pub tensor_names: Vec<String>,
    pub tensor_type_counts: BTreeMap<u32, u64>,
    pub header_sha256: String,
    pub header_bytes: u64,
    pub template_identity: Option<StringIdentity>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct StringIdentity {
    pub sha256: String,
    pub bytes: u64,
}

pub fn string_identity(text: &str) -> StringIdentity {
    StringIdentity {
        sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
        bytes: text.len() as u64,
    }
}

impl GgufHeader {
    /// Hashes cover exact parsed header/template bytes, never model weights.
    pub fn provenance(&self) -> serde_json::Value {
        serde_json::json!({
            "gguf_version": self.version,
            "architecture": self.architecture(),
            "header_sha256": self.header_sha256,
            "header_bytes": self.header_bytes,
            "quantization": {
                "file_type": self.u64("general.file_type"),
                "version": self.u64("general.quantization_version"),
                "tensor_type_counts": self.tensor_type_counts,
            },
            "chat_template": self.template_identity,
        })
    }
    pub fn str(&self, key: &str) -> Option<&str> {
        self.metadata.get(key).and_then(Scalar::as_str)
    }
    pub fn u64(&self, key: &str) -> Option<u64> {
        self.metadata.get(key).and_then(Scalar::as_u64)
    }
    pub fn bool(&self, key: &str) -> Option<bool> {
        self.metadata.get(key).and_then(Scalar::as_bool)
    }
    pub fn architecture(&self) -> Option<&str> {
        self.str("general.architecture")
    }
    /// `{arch}.{suffix}` lookup.
    pub fn arch_u64(&self, suffix: &str) -> Option<u64> {
        let arch = self.architecture()?;
        self.u64(&format!("{arch}.{suffix}"))
    }
    /// `{arch}.{suffix}` when stored as a per-layer integer array.
    pub fn arch_array(&self, suffix: &str) -> Option<&[i64]> {
        let arch = self.architecture()?;
        match self.metadata.get(&format!("{arch}.{suffix}"))? {
            Scalar::IntArray(values) => Some(values),
            _ => None,
        }
    }
    pub fn has_tensor(&self, name: &str) -> bool {
        self.tensor_names.iter().any(|t| t == name)
    }
    /// True for multimodal projector files (`mmproj`).
    pub fn is_projector(&self) -> bool {
        self.architecture() == Some("clip")
            || self.str("general.type") == Some("mmproj")
            || self.bool("clip.has_vision_encoder").is_some()
    }
    pub fn has_vision_encoder(&self) -> bool {
        self.bool("clip.has_vision_encoder") == Some(true)
    }
    pub fn chat_template(&self) -> Option<&str> {
        self.str("tokenizer.chat_template")
    }
    /// A retained default-template prefix mentions tools. Comments and literal
    /// text can match; this is a schema-offering hint, not a capability check.
    /// Generic runtime reports and successful tool execution are separate facts.
    pub fn template_mentions_tools(&self) -> bool {
        self.chat_template()
            .is_some_and(|t| t.contains("tools") || t.contains("tool_call"))
    }
    /// The chat template understands an `enable_thinking` switch (Qwen3 style).
    pub fn template_has_thinking_switch(&self) -> bool {
        self.chat_template()
            .is_some_and(|t| t.contains("enable_thinking"))
    }
    /// Tokenizer-only files ship the vocabulary without any weights.
    pub fn is_vocab_only(&self) -> bool {
        self.n_tensors == 0 || self.bool("general.vocab_only") == Some(true)
    }
    /// Encoder-only or pooled embedding models cannot chat.
    pub fn is_embedding_model(&self) -> bool {
        const ENCODERS: &[&str] = &[
            "bert",
            "nomic-bert",
            "nomic-bert-moe",
            "jina-bert-v2",
            "jina-bert-v3",
            "neo-bert",
            "modern-bert",
            "eurobert",
            "t5encoder",
            "gemma-embedding",
        ];
        self.architecture().is_some_and(|a| ENCODERS.contains(&a))
            || self.arch_u64("pooling_type").is_some_and(|v| v > 0)
    }
}

struct Reader<R: Read> {
    inner: R,
    consumed: u64,
    digest: Sha256,
}

impl<R: Read> Reader<R> {
    fn exact(&mut self, buf: &mut [u8]) -> Result<()> {
        self.consumed = self.consumed.saturating_add(buf.len() as u64);
        ensure!(
            self.consumed <= MAX_HEADER_BYTES,
            "GGUF metadata section is unreasonably large"
        );
        self.inner
            .read_exact(buf)
            .context("GGUF header ended early")?;
        self.digest.update(buf);
        Ok(())
    }
    fn u8(&mut self) -> Result<u8> {
        let mut b = [0u8; 1];
        self.exact(&mut b)?;
        Ok(b[0])
    }
    fn u16(&mut self) -> Result<u16> {
        let mut b = [0u8; 2];
        self.exact(&mut b)?;
        Ok(u16::from_le_bytes(b))
    }
    fn u32(&mut self) -> Result<u32> {
        let mut b = [0u8; 4];
        self.exact(&mut b)?;
        Ok(u32::from_le_bytes(b))
    }
    fn u64(&mut self) -> Result<u64> {
        let mut b = [0u8; 8];
        self.exact(&mut b)?;
        Ok(u64::from_le_bytes(b))
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_bits(self.u64()?))
    }
    fn skip(&mut self, mut n: u64) -> Result<()> {
        let mut buf = [0u8; 8192];
        while n > 0 {
            let take = n.min(buf.len() as u64) as usize;
            self.exact(&mut buf[..take])?;
            n -= take as u64;
        }
        Ok(())
    }
    fn string(&mut self) -> Result<String> {
        let len = self.u64()?;
        ensure!(len <= MAX_STRING_BYTES, "GGUF string is unreasonably long");
        let keep = (len as usize).min(KEEP_STRING_BYTES);
        let mut bytes = vec![0u8; keep];
        self.exact(&mut bytes)?;
        self.skip(len - keep as u64)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
    fn template_string(&mut self) -> Result<(String, StringIdentity)> {
        let len = self.u64()?;
        ensure!(len <= MAX_STRING_BYTES, "GGUF string is unreasonably long");
        let mut kept = Vec::with_capacity((len as usize).min(KEEP_STRING_BYTES));
        let mut digest = Sha256::new();
        let mut remaining = len;
        let mut buffer = [0u8; 8192];
        while remaining > 0 {
            let take = remaining.min(buffer.len() as u64) as usize;
            self.exact(&mut buffer[..take])?;
            digest.update(&buffer[..take]);
            let keep = take.min(KEEP_STRING_BYTES.saturating_sub(kept.len()));
            kept.extend_from_slice(&buffer[..keep]);
            remaining -= take as u64;
        }
        Ok((
            String::from_utf8_lossy(&kept).into_owned(),
            StringIdentity {
                sha256: format!("{:x}", digest.finalize()),
                bytes: len,
            },
        ))
    }
    fn scalar(&mut self, kind: u32) -> Result<Scalar> {
        Ok(match kind {
            0 => Scalar::U64(self.u8()? as u64),
            1 => Scalar::I64(self.u8()? as i8 as i64),
            2 => Scalar::U64(self.u16()? as u64),
            3 => Scalar::I64(self.u16()? as i16 as i64),
            4 => Scalar::U64(self.u32()? as u64),
            5 => Scalar::I64(self.u32()? as i32 as i64),
            6 => Scalar::F64(self.f32()? as f64),
            7 => Scalar::Bool(self.u8()? != 0),
            8 => Scalar::Str(self.string()?),
            9 => {
                let elem = self.u32()?;
                let count = self.u64()?;
                // Small integer/bool arrays are per-layer hyperparameters
                // (for example head_count_kv or sliding_window_pattern).
                if matches!(elem, 0..=5 | 7 | 10 | 11) && count <= MAX_KEPT_ARRAY {
                    let mut values = Vec::with_capacity(count as usize);
                    for _ in 0..count {
                        values.push(match self.scalar(elem)? {
                            Scalar::U64(v) => v as i64,
                            Scalar::I64(v) => v,
                            Scalar::Bool(v) => v as i64,
                            _ => 0,
                        });
                    }
                    Scalar::IntArray(values)
                } else {
                    self.skip_array(elem, count)?;
                    Scalar::Array(count)
                }
            }
            10 => Scalar::U64(self.u64()?),
            11 => Scalar::I64(self.u64()? as i64),
            12 => Scalar::F64(self.f64()?),
            other => bail!("Unknown GGUF value type {other}"),
        })
    }
    fn skip_array(&mut self, elem: u32, count: u64) -> Result<()> {
        let fixed = match elem {
            0 | 1 | 7 => Some(1u64),
            2 | 3 => Some(2),
            4..=6 => Some(4),
            10..=12 => Some(8),
            8 | 9 => None,
            other => bail!("Unknown GGUF array element type {other}"),
        };
        match fixed {
            Some(size) => self.skip(count.checked_mul(size).context("GGUF array too large")?),
            None => {
                for _ in 0..count {
                    if elem == 8 {
                        let len = self.u64()?;
                        ensure!(len <= MAX_STRING_BYTES, "GGUF string is unreasonably long");
                        self.skip(len)?;
                    } else {
                        let inner = self.u32()?;
                        let inner_count = self.u64()?;
                        self.skip_array(inner, inner_count)?;
                    }
                }
                Ok(())
            }
        }
    }
}

/// True when the file starts with the GGUF magic.
pub fn is_gguf(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).is_ok() && magic == *MAGIC
}

pub fn read_header(path: &Path) -> Result<GgufHeader> {
    let mut file = File::open(path).with_context(|| format!("Cannot open {}", path.display()))?;
    file.seek(SeekFrom::Start(0))?;
    let mut reader = Reader {
        inner: BufReader::with_capacity(1 << 20, file),
        consumed: 0,
        digest: Sha256::new(),
    };
    let mut magic = [0u8; 4];
    reader.exact(&mut magic)?;
    ensure!(magic == *MAGIC, "Not a GGUF file (missing GGUF magic)");
    let version = reader.u32()?;
    ensure!(
        (2..=3).contains(&version),
        "GGUF version {version} is not supported by the bundled runtime"
    );
    let n_tensors = reader.u64()?;
    let n_kv = reader.u64()?;
    ensure!(n_tensors <= MAX_TENSORS, "GGUF declares too many tensors");
    ensure!(n_kv <= MAX_KV, "GGUF declares too many metadata keys");
    let mut metadata = BTreeMap::new();
    let mut template_identity = None;
    for _ in 0..n_kv {
        let key = reader.string()?;
        let kind = reader.u32()?;
        if key == "tokenizer.chat_template" {
            template_identity = None;
        }
        let value = if key == "tokenizer.chat_template" && kind == 8 {
            let (text, identity) = reader.template_string()?;
            template_identity = Some(identity);
            Scalar::Str(text)
        } else {
            reader.scalar(kind)?
        };
        metadata.insert(key, value);
    }
    let mut tensor_names = Vec::with_capacity(n_tensors.min(4096) as usize);
    let mut tensor_type_counts = BTreeMap::new();
    for _ in 0..n_tensors {
        let name = reader.string()?;
        let dims = reader.u32()?;
        ensure!(dims <= 8, "GGUF tensor has too many dimensions");
        for _ in 0..dims {
            reader.u64()?;
        }
        let tensor_type = reader.u32()?;
        *tensor_type_counts.entry(tensor_type).or_insert(0) += 1;
        reader.u64()?; // offset
        tensor_names.push(name);
    }
    Ok(GgufHeader {
        version,
        n_tensors,
        metadata,
        tensor_names,
        tensor_type_counts,
        header_sha256: format!("{:x}", reader.digest.finalize()),
        header_bytes: reader.consumed,
        template_identity,
    })
}

/// Rough resident memory needed to run a text model: weights are mapped from
/// the file, plus the KV cache for `context` tokens, a compute buffer, an
/// optional projector, and fixed runtime overhead. Numbers are estimates, not
/// promises; the runtime reports the real failure when a load does not fit.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MemoryEstimate {
    pub weights_bytes: u64,
    pub kv_cache_bytes: u64,
    pub compute_bytes: u64,
    pub projector_bytes: u64,
    pub overhead_bytes: u64,
    pub total_bytes: u64,
    pub context_tokens: u64,
}

pub fn estimate_memory(
    header: &GgufHeader,
    weights_bytes: u64,
    projector_bytes: u64,
    context_tokens: u64,
) -> MemoryEstimate {
    let layers = header.arch_u64("block_count").unwrap_or(32);
    let embedding = header.arch_u64("embedding_length").unwrap_or(4096);
    let heads = header.arch_u64("attention.head_count").unwrap_or(32).max(1);
    let kv_heads = header
        .arch_u64("attention.head_count_kv")
        .filter(|v| *v > 0)
        .unwrap_or(heads);
    let kv_heads_per_layer = header.arch_array("attention.head_count_kv");
    let head_dim = header
        .arch_u64("attention.key_length")
        .filter(|v| *v > 0)
        .unwrap_or(embedding / heads);
    let value_dim = header
        .arch_u64("attention.value_length")
        .filter(|v| *v > 0)
        .unwrap_or(head_dim);
    // Sliding-window layers (when the file says which ones) only cache the
    // window plus one batch, with their own head sizes.
    let window = header
        .arch_u64("attention.sliding_window")
        .filter(|v| *v > 0);
    let swa_pattern = header.arch_array("attention.sliding_window_pattern");
    let swa_dims = header
        .arch_u64("attention.key_length_swa")
        .filter(|v| *v > 0)
        .unwrap_or(head_dim)
        .saturating_add(
            header
                .arch_u64("attention.value_length_swa")
                .filter(|v| *v > 0)
                .unwrap_or(value_dim),
        );
    // f16 K and V per layer.
    let mut kv_cache_bytes: u64 = 0;
    for layer in 0..layers.min(4096) {
        let heads_here = kv_heads_per_layer
            .and_then(|a| a.get(layer as usize))
            .map(|v| (*v).max(0) as u64)
            .unwrap_or(kv_heads);
        let swa = match (window, swa_pattern) {
            (Some(_), Some(pattern)) => pattern.get(layer as usize).is_some_and(|v| *v != 0),
            _ => false,
        };
        let (tokens, dims) = if swa {
            let window = window.unwrap_or(context_tokens);
            (context_tokens.min(window.saturating_add(512)), swa_dims)
        } else {
            (context_tokens, head_dim.saturating_add(value_dim))
        };
        kv_cache_bytes = kv_cache_bytes.saturating_add(
            heads_here
                .saturating_mul(dims)
                .saturating_mul(2)
                .saturating_mul(tokens),
        );
    }
    // Activations and scratch buffers scale with the batch and embedding size.
    let compute_bytes = embedding
        .saturating_mul(2048)
        .saturating_mul(4)
        .clamp(256 * 1024 * 1024, 2 * 1024 * 1024 * 1024);
    let overhead_bytes: u64 = 512 * 1024 * 1024
        + if projector_bytes > 0 {
            // Image encoding buffers.
            768 * 1024 * 1024
        } else {
            0
        };
    let total_bytes = weights_bytes
        .saturating_add(kv_cache_bytes)
        .saturating_add(compute_bytes)
        .saturating_add(projector_bytes)
        .saturating_add(overhead_bytes);
    MemoryEstimate {
        weights_bytes,
        kv_cache_bytes,
        compute_bytes,
        projector_bytes,
        overhead_bytes,
        total_bytes,
        context_tokens,
    }
}

/// Synthetic GGUF writer for unit and integration tests. Nothing in the
/// catalog, runtime, or readiness code calls it.
#[doc(hidden)]
pub mod test_support {
    use std::io::Write;

    pub enum V<'a> {
        U32(u32),
        U64(u64),
        Str(&'a str),
        Bool(bool),
        StrArray(&'a [&'a str]),
        U32Array(&'a [u32]),
        BoolArray(&'a [bool]),
    }

    fn put_string(out: &mut Vec<u8>, s: &str) {
        out.extend_from_slice(&(s.len() as u64).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
    }

    pub fn write_gguf(path: &std::path::Path, kv: &[(&str, V<'_>)], tensors: &[&str]) {
        write_gguf_padded(path, kv, tensors, 4096)
    }

    /// [`write_gguf`] with `padding` bytes of pretend weights after the header.
    pub fn write_gguf_padded(
        path: &std::path::Path,
        kv: &[(&str, V<'_>)],
        tensors: &[&str],
        padding: usize,
    ) {
        let mut out = Vec::new();
        out.extend_from_slice(b"GGUF");
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&(tensors.len() as u64).to_le_bytes());
        out.extend_from_slice(&(kv.len() as u64).to_le_bytes());
        for (key, value) in kv {
            put_string(&mut out, key);
            match value {
                V::U32(v) => {
                    out.extend_from_slice(&4u32.to_le_bytes());
                    out.extend_from_slice(&v.to_le_bytes());
                }
                V::U64(v) => {
                    out.extend_from_slice(&10u32.to_le_bytes());
                    out.extend_from_slice(&v.to_le_bytes());
                }
                V::Str(v) => {
                    out.extend_from_slice(&8u32.to_le_bytes());
                    put_string(&mut out, v);
                }
                V::Bool(v) => {
                    out.extend_from_slice(&7u32.to_le_bytes());
                    out.push(*v as u8);
                }
                V::StrArray(items) => {
                    out.extend_from_slice(&9u32.to_le_bytes());
                    out.extend_from_slice(&8u32.to_le_bytes());
                    out.extend_from_slice(&(items.len() as u64).to_le_bytes());
                    for item in *items {
                        put_string(&mut out, item);
                    }
                }
                V::U32Array(items) => {
                    out.extend_from_slice(&9u32.to_le_bytes());
                    out.extend_from_slice(&4u32.to_le_bytes());
                    out.extend_from_slice(&(items.len() as u64).to_le_bytes());
                    for item in *items {
                        out.extend_from_slice(&item.to_le_bytes());
                    }
                }
                V::BoolArray(items) => {
                    out.extend_from_slice(&9u32.to_le_bytes());
                    out.extend_from_slice(&7u32.to_le_bytes());
                    out.extend_from_slice(&(items.len() as u64).to_le_bytes());
                    for item in *items {
                        out.push(*item as u8);
                    }
                }
            }
        }
        for name in tensors {
            put_string(&mut out, name);
            out.extend_from_slice(&2u32.to_le_bytes());
            out.extend_from_slice(&4u64.to_le_bytes());
            out.extend_from_slice(&4u64.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&0u64.to_le_bytes());
        }
        // Pretend weights so the file has a size.
        out.resize(out.len() + padding, 0);
        std::fs::File::create(path)
            .unwrap()
            .write_all(&out)
            .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{write_gguf, V};

    #[test]
    fn provenance_hashes_the_full_template_and_exact_header_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("long.gguf");
        let template = format!(
            "{}tail beyond retained prefix",
            "x".repeat(KEEP_STRING_BYTES)
        );
        write_gguf(
            &path,
            &[
                ("general.architecture", V::Str("qwen3")),
                ("general.file_type", V::U32(15)),
                ("general.quantization_version", V::U32(2)),
                ("tokenizer.chat_template", V::Str(&template)),
            ],
            &["token_embd.weight", "output.weight"],
        );
        let header = read_header(&path).unwrap();
        assert_eq!(header.chat_template().unwrap().len(), KEEP_STRING_BYTES);
        assert_eq!(
            header.template_identity.as_ref().unwrap(),
            &string_identity(&template)
        );
        assert_ne!(
            header.template_identity.as_ref().unwrap(),
            &string_identity(header.chat_template().unwrap())
        );
        let bytes = std::fs::read(&path).unwrap();
        assert!(header.header_bytes < bytes.len() as u64);
        assert_eq!(
            header.header_sha256,
            format!(
                "{:x}",
                Sha256::digest(&bytes[..header.header_bytes as usize])
            )
        );
        let receipt = header.provenance();
        assert_eq!(receipt["quantization"]["file_type"], 15);
        assert_eq!(receipt["quantization"]["version"], 2);
        assert_eq!(receipt["quantization"]["tensor_type_counts"]["0"], 2);
    }
    use super::*;

    #[test]
    fn reads_metadata_and_tensor_names_without_loading_arrays() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.gguf");
        let vocab: Vec<&str> = (0..2000).map(|_| "tok").collect();
        write_gguf(
            &path,
            &[
                ("general.architecture", V::Str("qwen3")),
                ("general.name", V::Str("Qwen3 14B")),
                ("qwen3.context_length", V::U32(40960)),
                ("qwen3.block_count", V::U32(40)),
                ("qwen3.embedding_length", V::U32(5120)),
                ("qwen3.attention.head_count", V::U32(40)),
                ("qwen3.attention.head_count_kv", V::U32(8)),
                ("qwen3.attention.key_length", V::U32(128)),
                ("general.quantization_version", V::U64(2)),
                ("tokenizer.ggml.tokens", V::StrArray(&vocab)),
                (
                    "tokenizer.chat_template",
                    V::Str("{% if tools %}...{% endif %}"),
                ),
            ],
            &["token_embd.weight", "output.weight"],
        );
        let header = read_header(&path).unwrap();
        assert_eq!(header.version, 3);
        assert_eq!(header.architecture(), Some("qwen3"));
        assert_eq!(header.arch_u64("context_length"), Some(40960));
        assert_eq!(header.u64("general.quantization_version"), Some(2));
        assert_eq!(
            header.metadata.get("tokenizer.ggml.tokens"),
            Some(&Scalar::Array(2000))
        );
        assert!(header.has_tensor("token_embd.weight"));
        assert!(header.template_mentions_tools());
        assert!(!header.is_projector());
        let estimate = estimate_memory(&header, 9_000_000_000, 0, 8192);
        // 40 layers * 8 kv heads * (128+128) * 2 bytes * 8192 tokens.
        assert_eq!(estimate.kv_cache_bytes, 40 * 8 * 256 * 2 * 8192);
        assert!(estimate.total_bytes > 9_000_000_000);
    }

    #[test]
    fn per_layer_kv_heads_and_sliding_window_layers_shrink_the_kv_estimate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.gguf");
        // Four layers: three sliding-window layers, one global layer.
        write_gguf(
            &path,
            &[
                ("general.architecture", V::Str("gemma4")),
                ("gemma4.block_count", V::U32(4)),
                ("gemma4.embedding_length", V::U32(1024)),
                ("gemma4.attention.head_count", V::U32(16)),
                ("gemma4.attention.head_count_kv", V::U32Array(&[8, 8, 8, 2])),
                ("gemma4.attention.key_length", V::U32(512)),
                ("gemma4.attention.value_length", V::U32(512)),
                ("gemma4.attention.key_length_swa", V::U32(256)),
                ("gemma4.attention.value_length_swa", V::U32(256)),
                ("gemma4.attention.sliding_window", V::U32(1024)),
                (
                    "gemma4.attention.sliding_window_pattern",
                    V::BoolArray(&[true, true, true, false]),
                ),
            ],
            &["token_embd.weight"],
        );
        let header = read_header(&path).unwrap();
        assert_eq!(
            header.arch_array("attention.head_count_kv"),
            Some(&[8i64, 8, 8, 2][..])
        );
        let estimate = estimate_memory(&header, 0, 0, 16384);
        let swa = 3 * 8 * 512 * 2 * (1024 + 512);
        let global = 2 * 1024 * 2 * 16384;
        assert_eq!(estimate.kv_cache_bytes, swa + global);
    }

    #[test]
    fn detects_projectors_and_rejects_non_gguf() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("mmproj.gguf");
        write_gguf(
            &proj,
            &[
                ("general.architecture", V::Str("clip")),
                ("clip.has_vision_encoder", V::Bool(true)),
                ("clip.vision.projector_type", V::Str("gemma4uv")),
            ],
            &["v.patch_embd.weight"],
        );
        let header = read_header(&proj).unwrap();
        assert!(header.is_projector());
        assert!(header.has_vision_encoder());
        let bad = dir.path().join("bad.bin");
        std::fs::write(&bad, b"NOTGGUF").unwrap();
        assert!(!is_gguf(&bad));
        assert!(read_header(&bad).is_err());
        let v1 = dir.path().join("v1.gguf");
        let mut bytes = b"GGUF".to_vec();
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 32]);
        std::fs::write(&v1, bytes).unwrap();
        assert!(read_header(&v1)
            .unwrap_err()
            .to_string()
            .contains("version 1"));
    }
}
