//! Narrow compatibility profiles for known tool-trained GGUF/template pairs.
//! Selection uses file metadata, never a filename or the user's model label.
//! It does not authenticate weights or establish model quality. The runtime
//! must confirm the selected template before a prepared model receives tools.
use crate::gguf::{self, GgufHeader, Scalar};
use serde_json::{json, Value};

const HERMES_DEFAULT_SHA256: &str =
    "a805e50fed68938a076b07e2e602639611b50b1ced0e50f11eb92f1ba25be4dc";
const HERMES_TOOL_SHA256: &str = "7ce09d55d3690e06c70b5c07228cc7e8c99c43a23cdc027762be4011efcdea6c";
const LLAMA_COMMIT: &str = "18f9f7bef960b76b693d8dcbb33cbbd6148c1631";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Hermes2ProLlama3,
}

impl Profile {
    pub fn id(self) -> &'static str {
        match self {
            Self::Hermes2ProLlama3 => "hermes-2-pro-llama-3-8b-tool-use-v1",
        }
    }

    pub fn template(self) -> &'static str {
        match self {
            Self::Hermes2ProLlama3 => {
                include_str!("local_templates/hermes-2-pro-llama-3-8b-tool-use.jinja")
            }
        }
    }

    pub fn identity(self) -> gguf::StringIdentity {
        gguf::string_identity(self.template())
    }

    pub fn reported_matches(self, props: &Value) -> bool {
        props
            .get("chat_template")
            .and_then(Value::as_str)
            .is_some_and(|template| gguf::string_identity(template) == self.identity())
    }

    pub fn provenance(self) -> Value {
        json!({
            "profile": self.id(),
            "source": "bundled_llama_cpp_template",
            "source_commit": LLAMA_COMMIT,
            "source_sha256": HERMES_TOOL_SHA256,
            "template": self.identity(),
            "selection": "gguf_metadata_and_default_template_identity",
            "runtime_template_verified": true,
        })
    }
}

fn exact_token_id(header: &GgufHeader, key: &str, expected: u64) -> bool {
    matches!(header.metadata.get(key), Some(Scalar::U64(value)) if *value == expected)
}

/// Hermes' ordinary ChatML template does not describe functions. The pinned
/// llama.cpp docs prescribe a separate tool_use template for this model.
/// Unknown variants, changed templates and Rhea receive no implicit override.
pub fn select(header: &GgufHeader) -> Option<Profile> {
    (header.architecture() == Some("llama")
        && header.str("general.name") == Some("Hermes-2-Pro-Llama-3-8B")
        && header.str("tokenizer.ggml.model") == Some("gpt2")
        && header.str("tokenizer.ggml.pre") == Some("llama-bpe")
        && exact_token_id(header, "tokenizer.ggml.bos_token_id", 128000)
        && exact_token_id(header, "tokenizer.ggml.eos_token_id", 128003)
        && exact_token_id(header, "tokenizer.ggml.padding_token_id", 128001)
        && header.template_identity.as_ref().is_some_and(|identity| {
            identity.bytes == 209 && identity.sha256 == HERMES_DEFAULT_SHA256
        })
        // Do not replace an explicitly supplied named template.
        && !header.metadata.keys().any(|key| key.starts_with("tokenizer.chat_template.")))
    .then_some(Profile::Hermes2ProLlama3)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> GgufHeader {
        let mut h = GgufHeader::default();
        for (key, value) in [
            ("general.architecture", "llama"),
            ("general.name", "Hermes-2-Pro-Llama-3-8B"),
            ("tokenizer.ggml.model", "gpt2"),
            ("tokenizer.ggml.pre", "llama-bpe"),
        ] {
            h.metadata.insert(key.into(), Scalar::Str(value.into()));
        }
        for (key, value) in [
            ("tokenizer.ggml.bos_token_id", 128000),
            ("tokenizer.ggml.eos_token_id", 128003),
            ("tokenizer.ggml.padding_token_id", 128001),
        ] {
            h.metadata.insert(key.into(), Scalar::U64(value));
        }
        h.template_identity = Some(gguf::StringIdentity {
            bytes: 209,
            sha256: HERMES_DEFAULT_SHA256.into(),
        });
        h
    }

    #[test]
    fn hermes_profile_requires_every_known_metadata_condition() {
        let h = header();
        assert_eq!(select(&h), Some(Profile::Hermes2ProLlama3));
        for key in h.metadata.keys() {
            let mut changed = h.clone();
            changed.metadata.remove(key);
            assert_eq!(select(&changed), None, "missing {key}");
            changed
                .metadata
                .insert(key.clone(), Scalar::Str("other".into()));
            assert_eq!(select(&changed), None, "changed {key}");
        }
        for key in [
            "tokenizer.ggml.bos_token_id",
            "tokenizer.ggml.eos_token_id",
            "tokenizer.ggml.padding_token_id",
        ] {
            let mut changed = h.clone();
            changed.metadata.insert(key.into(), Scalar::U64(42));
            assert_eq!(select(&changed), None, "wrong {key}");
        }
        for identity in [
            None,
            Some(gguf::StringIdentity {
                bytes: 210,
                sha256: HERMES_DEFAULT_SHA256.into(),
            }),
            Some(gguf::StringIdentity {
                bytes: 209,
                sha256: "other".into(),
            }),
        ] {
            let mut changed = h.clone();
            changed.template_identity = identity;
            assert_eq!(select(&changed), None);
        }
        let mut named = h.clone();
        named.metadata.insert(
            "tokenizer.chat_template.tool_use".into(),
            Scalar::Str("custom".into()),
        );
        assert_eq!(select(&named), None);
        let mut rhea = h;
        rhea.metadata
            .insert("general.architecture".into(), Scalar::Str("qwen3".into()));
        rhea.metadata.insert(
            "general.name".into(),
            Scalar::Str("Rhea 4B Coding Max".into()),
        );
        rhea.template_identity = None;
        assert_eq!(select(&rhea), None);
    }

    #[test]
    fn bundled_template_is_exact_and_runtime_must_report_it() {
        let profile = Profile::Hermes2ProLlama3;
        assert_eq!(profile.identity().sha256, HERMES_TOOL_SHA256);
        assert_eq!(profile.identity().bytes, 5004);
        assert!(profile.reported_matches(&json!({"chat_template":profile.template()})));
        assert!(!profile.reported_matches(&Value::Null));
        assert!(!profile.reported_matches(&json!({"chat_template":"ignored override"})));
        assert!(
            !profile.reported_matches(&json!({"chat_template":format!("{} ",profile.template())}))
        );
    }
}
