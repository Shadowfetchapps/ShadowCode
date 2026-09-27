//! Read-only probe of the local engine on this computer: runtime readiness,
//! hardware, and the Ollama store as the catalog sees them. Starts no model.
//!
//! cargo run -p shadowcode-core --example probe_local -- [/path/to/llama-server]
use serde_json::json;
use shadowcode_core::{local_engine, ollama_store};

fn main() {
    let binary = std::env::args().nth(1).unwrap_or_default();
    let config = local_engine::LocalEngineConfig {
        llama_binary: binary,
        ..Default::default()
    };
    let catalog = local_engine::catalog(&config);
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "hardware": catalog["hardware"],
            "runtime": catalog["runtime"],
            "ollama_store": catalog["ollama_store"],
        }))
        .unwrap()
    );
    let Some(root) = ollama_store::discover() else {
        return;
    };
    for model in ollama_store::list(&root.path) {
        let Ok(header) = local_engine::header(&model.model) else {
            println!("{}: unreadable header", model.tag);
            continue;
        };
        let projector = model
            .projector
            .as_ref()
            .and_then(|p| local_engine::header(p).ok());
        println!(
            "{} arch={:?} ctx_train={:?} embd={:?} template={} tools_template_hint={} thinking_switch={} projector={:?}",
            model.tag,
            header.architecture(),
            header.arch_u64("context_length"),
            header.arch_u64("embedding_length"),
            header.chat_template().map(str::len).unwrap_or(0),
            header.template_mentions_tools(),
            header.template_has_thinking_switch(),
            projector.map(|p| (
                p.architecture().map(str::to_owned),
                p.u64("clip.vision.projection_dim"),
                p.has_vision_encoder()
            )),
        );
        if std::env::var_os("PROBE_KEYS").is_some() {
            let prefix = format!("{}.", header.architecture().unwrap_or(""));
            for (key, value) in header
                .metadata
                .iter()
                .filter(|(k, _)| k.starts_with(&prefix))
            {
                println!("    {key} = {value:?}");
            }
        }
    }
}
