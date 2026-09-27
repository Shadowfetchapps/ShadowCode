The Hermes tool-use template is copied byte-for-byte from llama.cpp commit
`18f9f7bef960b76b693d8dcbb33cbbd6148c1631`, the runtime pin used when this profile
was introduced:

https://github.com/ggml-org/llama.cpp/blob/18f9f7bef960b76b693d8dcbb33cbbd6148c1631/models/templates/NousResearch-Hermes-2-Pro-Llama-3-8B-tool_use.jinja

Size: 5,004 bytes. SHA-256:
`7ce09d55d3690e06c70b5c07228cc7e8c99c43a23cdc027762be4011efcdea6c`.
The upstream MIT license is retained in `LICENSE.llama.cpp`.

The same pinned runtime's `docs/function-calling.md` prescribes this template
for `bartowski/Hermes-2-Pro-Llama-3-8B-GGUF`. Its model author documents a
separate `tool_use` template:
https://huggingface.co/NousResearch/Hermes-2-Pro-Llama-3-8B#prompt-format-for-function-calling

Selection requires GGUF architecture `llama`, embedded general.name
`Hermes-2-Pro-Llama-3-8B`, tokenizer model `gpt2`, pre-tokenizer `llama-bpe`,
BOS/EOS/padding IDs 128000/128003/128001, and the exact known 209-byte default
ChatML template (SHA-256
`a805e50fed68938a076b07e2e602639611b50b1ced0e50f11eb92f1ba25be4dc`). Named
template variants prevent this override. File names and user labels do not
select profiles. These metadata checks do not authenticate model weights.

The template is embedded in ShadowCode and passed to its child runtime; model
files and Ollama templates are not edited. The runtime must report the exact
selected template before preparation succeeds. Rhea has no compatibility
profile. Fixture protocol coverage does not establish live coding quality.
